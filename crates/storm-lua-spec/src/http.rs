//! ゲーム互換のHTTPリクエスト／レスポンス管理。ネットワーククライアント、URLリゾルバ、スケジューラは含みません。
//!
//! 実ゲームの契約に合わせ、ポート・リクエスト・応答への検証は行いません。
//! ポートはLua側の数値変換で得た符号付き整数を受け取り、下位16bitを保持します。
//! リクエストbytesはそのまま保持し、応答bytesもサイズで拒否しません。
//! ここで拒否するのは、世代・IDが一致しないトークンによる精算だけです。
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// 不透明なレスポンス識別子。リセット後や異なるVMからのトークンは一致しません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpToken {
    /// 一意のキュー世代。JS側ではbigintまたはタグ付き10進数文字列として受け渡されます。
    pub generation: u64,
    /// この世代内で単調増加するリクエストID。
    pub id: u32,
}
/// ホストトランスポートに渡されるリクエスト所有データ。エンジン自体はリクエストを送信しません。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// レスポンス／キャンセル用トークン。
    pub token: HttpToken,
    /// 接続先ポート。Lua数値を符号付き整数へ変換した値の下位16bitであり、範囲検証はしません。
    pub port: u16,
    /// リクエストbytes。先頭文字・禁止文字・長さの検証をせず、そのまま保持します。
    pub request: Vec<u8>,
}
/// 不正なホストトランスポート操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// リクエスト識別子の上限を使い果たしました。
    Limit,
    /// 失効したトークン、別VMのトークン、重複トークン、または未送信のトークンです。
    Unknown,
}
impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Limit => "HTTP request identifier budget exhausted",
            Self::Unknown => "stale, foreign, duplicate or undelivered HTTP token",
        })
    }
}
impl std::error::Error for HttpError {}
/// レスポンス待ちの取得済みリクエストを含め、未処理リクエストを追跡します。件数の上限は設けません。
pub struct HttpQueue {
    generation: u64,
    next: u32,
    pending: BTreeMap<u32, (HttpRequest, bool)>,
    outbox: VecDeque<u32>,
}
impl HttpQueue {
    /// 独立した世代を作成します。再作成すると以前のすべてのトークンが無効化されます。
    pub fn new() -> Result<Self, HttpError> {
        let generation = NEXT_GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| HttpError::Limit)?;
        Ok(Self {
            generation,
            next: 1,
            pending: BTreeMap::new(),
            outbox: VecDeque::new(),
        })
    }
    /// リクエストをキューに追加します。ポートは下位16bitへ切り捨て、リクエストbytesは検証せず保持します。ネットワークポリシーはホスト側の責務です。
    pub fn request(&mut self, port: i64, request: &[u8]) -> Result<(), HttpError> {
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or(HttpError::Limit)?;
        let request = HttpRequest {
            token: HttpToken {
                generation: self.generation,
                id,
            },
            port: port as u16,
            request: request.to_vec(),
        };
        self.pending.insert(id, (request, false));
        self.outbox.push_back(id);
        Ok(())
    }
    /// 新規にキューイングされたリクエストのコピーを1度だけ取り出し、レスポンス管理情報を保持します。
    pub fn drain(&mut self) -> Vec<HttpRequest> {
        let mut requests = Vec::new();
        for id in self.outbox.drain(..) {
            if let Some((request, sent)) = self.pending.get_mut(&id) {
                *sent = true;
                requests.push(request.clone());
            }
        }
        requests
    }
    /// トークンが検証された後にのみ、送信済みリクエストを消費します。応答bytesのサイズは検証しません。
    pub fn reply(&mut self, token: HttpToken, _bytes: &[u8]) -> Result<HttpRequest, HttpError> {
        self.consume(token)
    }
    /// トランスポート失敗後に送信済みリクエストを明示的にキャンセルします（偽のLuaレスポンスは生成しません）。
    pub fn cancel(&mut self, token: HttpToken) -> Result<(), HttpError> {
        self.consume(token).map(|_| ())
    }
    fn consume(&mut self, token: HttpToken) -> Result<HttpRequest, HttpError> {
        if token.generation != self.generation
            || !self.pending.get(&token.id).is_some_and(|(_, sent)| *sent)
        {
            return Err(HttpError::Unknown);
        }
        self.pending
            .remove(&token.id)
            .map(|(request, _)| request)
            .ok_or(HttpError::Unknown)
    }
}
