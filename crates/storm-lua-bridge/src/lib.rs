//! 共有アダプタ機構。Lua、グラフィックス、unsafeな参照外し、ユーザースケジューラを含みません。
use std::{cell::RefCell, collections::BTreeMap, fmt};
use storm_lua_spec::{abi::status, draw::ScreenError};

/// 外部関数呼び出し（FFI）の結果コード。中断（Suspension）やコールバック不在は正常な結果として扱われます。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 正常に完了。
    Ok = status::OK,
    /// Luaのコンパイル／実行時エラー。
    Lua = status::LUA,
    /// リソース上限を超過。
    Limit = status::LIMIT,
    /// 不明、失効、または破棄済みのハンドル。
    InvalidHandle = status::INVALID_HANDLE,
    /// 不正なホスト引数またはバイナリデータ。
    InvalidArgument = status::INVALID_ARGUMENT,
    /// 再入操作または中断状態のコールバック。
    Busy = status::BUSY,
    /// 必要な機能またはプロバイダが利用不可。
    Unsupported = status::UNSUPPORTED,
    /// コールバックが中断中（フレーム／tick完了ではありません）。
    Suspended = status::SUSPENDED,
    /// 指定された名前のコールバックが存在しない。
    Missing = status::MISSING,
    /// VMが故障状態でありリセットが必要。
    Failed = status::FAILED,
    /// ホスト提供のサービスが失敗。
    Host = status::HOST,
}
/// アダプタエラー（ドメイン固有のバックエンドエラーとは分離）。
#[derive(Debug)]
pub struct BridgeError {
    /// 結果カテゴリ。
    pub status: Status,
    /// UTF-8の診断メッセージ。
    pub message: String,
    /// アダプタが定義する構造化された補足情報（UTF-8のJSON）。ない場合はNone。
    pub detail: Option<String>,
}
impl BridgeError {
    /// 明示的なアダプタエラーを構築します。
    pub fn new(status: Status, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            detail: None,
        }
    }
    /// 構造化された補足情報を付けます。メッセージは変更しません。
    #[must_use]
    pub fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
}
impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for BridgeError {}
impl From<ScreenError> for BridgeError {
    fn from(error: ScreenError) -> Self {
        Self::new(
            match &error {
                ScreenError::LimitExceeded => Status::Limit,
                ScreenError::MissingMapProvider => Status::Unsupported,
                ScreenError::Host(_) => Status::Host,
                _ => Status::InvalidArgument,
            },
            error.to_string(),
        )
    }
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}
/// 世代付きハンドル: 12ビットのインデックスと20ビットの世代。枯渇したスロットは引退します。
pub struct Registry<T> {
    slots: Vec<Slot<T>>,
}
impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}
impl<T> Registry<T> {
    /// 古いハンドル値を再利用せずに新しいオブジェクトを登録します。
    pub fn insert(&mut self, value: T) -> Result<u32, BridgeError> {
        let vacant = self
            .slots
            .iter()
            .position(|slot| slot.value.is_none() && slot.generation < (1 << 20));
        let index = if let Some(index) = vacant {
            self.slots[index].value = Some(value);
            index
        } else {
            if self.slots.len() >= 4095 {
                return Err(BridgeError::new(Status::Limit, "handle registry is full"));
            }
            self.slots
                .try_reserve(1)
                .map_err(|_| BridgeError::new(Status::Limit, "cannot allocate handle slot"))?;
            self.slots.push(Slot {
                generation: 1,
                value: Some(value),
            });
            self.slots.len() - 1
        };
        Ok((self.slots[index].generation << 12) | (index as u32 + 1))
    }
    fn index(&self, handle: u32) -> Result<usize, BridgeError> {
        let low = handle & 4095;
        if low == 0 {
            return Err(BridgeError::new(Status::InvalidHandle, "invalid handle"));
        }
        let index = (low - 1) as usize;
        if self
            .slots
            .get(index)
            .is_none_or(|slot| slot.generation != handle >> 12 || slot.value.is_none())
        {
            return Err(BridgeError::new(
                Status::InvalidHandle,
                "stale or unknown handle",
            ));
        }
        Ok(index)
    }
    /// 世代の検証後、既存のオブジェクトを可変借用します。
    pub fn get_mut(&mut self, handle: u32) -> Result<&mut T, BridgeError> {
        let index = self.index(handle)?;
        self.slots[index]
            .value
            .as_mut()
            .ok_or_else(|| BridgeError::new(Status::InvalidHandle, "disposed handle"))
    }
    /// オブジェクトを破棄して世代を進めます。二重破棄は拒否されます。
    pub fn remove(&mut self, handle: u32) -> Result<(), BridgeError> {
        let index = self.index(handle)?;
        self.slots[index].value = None;
        self.slots[index].generation += 1;
        Ok(())
    }
}
struct Uploads {
    buffers: BTreeMap<usize, Box<[u8]>>,
    bytes: usize,
}
thread_local! {
    static UPLOADS:RefCell<Uploads>=const{RefCell::new(Uploads{buffers:BTreeMap::new(),bytes:0})};
    static DIAGNOSTIC:RefCell<(Status,Vec<u8>)>=const{RefCell::new((Status::Ok,Vec::new()))};
    static DETAIL:RefCell<Vec<u8>>=const{RefCell::new(Vec::new())};
    static RESPONSE:RefCell<Vec<u8>>=const{RefCell::new(Vec::new())};
}
/// 状態変更または照会操作の前に前回の診断エラーをクリアします。
pub fn clear_error() {
    DIAGNOSTIC.with(|last| *last.borrow_mut() = (Status::Ok, Vec::new()));
    DETAIL.with(|last| last.borrow_mut().clear());
}
/// 返されたエラーを記録します。取得呼び出しによってクリアされることはありません。
pub fn record(error: BridgeError) {
    DETAIL.with(|last| *last.borrow_mut() = error.detail.unwrap_or_default().into_bytes());
    DIAGNOSTIC.with(|last| *last.borrow_mut() = (error.status, error.message.into_bytes()));
}
/// 診断情報を保持しつつ、Resultをステータスコードに変換します。
pub fn call(operation: impl FnOnce() -> Result<Status, BridgeError>) -> i32 {
    clear_error();
    match operation() {
        Ok(status) => status as i32,
        Err(error) => {
            let status = error.status;
            record(error);
            status as i32
        }
    }
}
/// ポインタまたはハンドルの照会を変換します。0はエラーの番兵値として予約されています。
pub fn value(operation: impl FnOnce() -> Result<usize, BridgeError>) -> usize {
    clear_error();
    match operation() {
        Ok(value) => value,
        Err(error) => {
            record(error);
            0
        }
    }
}
/// このモジュールインスタンスが所有する上限付きステージングメモリ確保。長さ0の要求は拒否されます。
pub fn allocate(length: u32) -> usize {
    value(|| {
        let length = length as usize;
        if length == 0 || length > 16 * 1024 * 1024 {
            return Err(BridgeError::new(
                Status::InvalidArgument,
                "invalid upload length",
            ));
        }
        UPLOADS.with(|uploads| {
            let mut uploads = uploads
                .try_borrow_mut()
                .map_err(|_| BridgeError::new(Status::Busy, "upload arena is borrowed"))?;
            if uploads.bytes + length > 64 * 1024 * 1024 || uploads.buffers.len() >= 4096 {
                return Err(BridgeError::new(
                    Status::Limit,
                    "upload arena limit exceeded",
                ));
            }
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(length)
                .map_err(|_| BridgeError::new(Status::Limit, "upload allocation failed"))?;
            buffer.resize(length, 0);
            let mut buffer = buffer.into_boxed_slice();
            let pointer = buffer.as_mut_ptr() as usize;
            uploads.bytes += length;
            uploads.buffers.insert(pointer, buffer);
            Ok(pointer)
        })
    })
}
/// 正確に1つのステージングメモリ確保を解放します。任意のホストポインタを参照外すことは決してありません。
pub fn free(pointer: usize) -> i32 {
    call(|| {
        UPLOADS.with(|uploads| {
            let mut uploads = uploads
                .try_borrow_mut()
                .map_err(|_| BridgeError::new(Status::Busy, "upload arena is borrowed"))?;
            let buffer = uploads
                .buffers
                .remove(&pointer)
                .ok_or_else(|| BridgeError::new(Status::InvalidHandle, "unknown upload pointer"))?;
            uploads.bytes -= buffer.len();
            Ok(Status::Ok)
        })
    })
}
/// 独立して検証された論理長を持つ、登録済みのアップロード領域のみを借用します。
pub fn with_upload<T>(
    pointer: usize,
    length: u32,
    read: impl FnOnce(&[u8]) -> Result<T, BridgeError>,
) -> Result<T, BridgeError> {
    UPLOADS.with(|uploads| {
        let uploads = uploads
            .try_borrow()
            .map_err(|_| BridgeError::new(Status::Busy, "upload arena is mutably borrowed"))?;
        let bytes = uploads
            .buffers
            .get(&pointer)
            .and_then(|bytes| bytes.get(..length as usize))
            .ok_or_else(|| {
                BridgeError::new(
                    Status::InvalidArgument,
                    "unregistered or out-of-range upload",
                )
            })?;
        read(bytes)
    })
}
/// 所有権を持つコールドパスレスポンスを格納します。置換時に古いレスポンスビューは失効します。
pub fn respond(bytes: Vec<u8>) -> Result<(), BridgeError> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(BridgeError::new(Status::Limit, "response is too large"));
    }
    RESPONSE.with(|response| *response.borrow_mut() = bytes);
    Ok(())
}
/// エラーバッファをクリアせずにエラーカテゴリを取得します。
pub fn error_status() -> i32 {
    DIAGNOSTIC.with(|last| last.borrow().0 as i32)
}
/// エラーUTF-8バッファのポインタ。次の通常操作まで有効です。
pub fn error_ptr() -> usize {
    DIAGNOSTIC.with(|last| last.borrow().1.as_ptr() as usize)
}
/// エラーUTF-8のバイト長。
pub fn error_len() -> usize {
    DIAGNOSTIC.with(|last| last.borrow().1.len())
}
/// エラー補足情報のUTF-8 JSONのポインタ。次の通常操作まで有効です。
pub fn error_detail_ptr() -> usize {
    DETAIL.with(|last| last.borrow().as_ptr() as usize)
}
/// エラー補足情報のバイト長。補足情報がない場合は0です。
pub fn error_detail_len() -> usize {
    DETAIL.with(|last| last.borrow().len())
}
/// レスポンスバッファのポインタ。次のレスポンス生成操作まで有効です。
pub fn response_ptr() -> usize {
    RESPONSE.with(|last| last.borrow().as_ptr() as usize)
}
/// レスポンスのバイト長。
pub fn response_len() -> usize {
    RESPONSE.with(|last| last.borrow().len())
}

/// 明示的なシンボル名を用いて、1つのアダプタ向けに同一の所有権／診断用エクスポート関数を生成します。
#[macro_export]
macro_rules! memory_exports {
    ($alloc:ident,$free:ident,$status:ident,$error_ptr:ident,$error_len:ident,$result_ptr:ident,$result_len:ident) => {
        /// 上限付きのホストアップロードバッファを確保します。
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn $alloc(length: u32) -> usize {
            $crate::allocate(length)
        }
        /// 登録済みのアップロード領域を解放します。未知のポインタや二重解放は拒否されます。
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn $free(pointer: usize) -> i32 {
            $crate::free(pointer)
        }
        /// 直前のエラーコード。診断情報はクリアされません。
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn $status() -> i32 {
            $crate::error_status()
        }
        /// 直前のエラーのUTF-8バイト列を参照します。
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn $error_ptr() -> usize {
            $crate::error_ptr()
        }
        /// 直前のエラーのバイト長。
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn $error_len() -> usize {
            $crate::error_len()
        }
        /// 直前のコールドレスポンスを参照します。
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn $result_ptr() -> usize {
            $crate::response_ptr()
        }
        /// コールドレスポンスのバイト長。
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn $result_len() -> usize {
            $crate::response_len()
        }
    };
}
