//! 初期化およびyield可能性検査のための、バックエンド限定の狭いunsafe境界。
#![allow(unsafe_code)]
use mlua::{ffi, Lua, LuaOptions, StdLib, Thread};
use std::ptr::NonNull;

pub(crate) fn create_lua() -> mlua::Result<Lua> {
    let libraries = StdLib::TABLE | StdLib::STRING | StdLib::MATH;
    #[cfg(feature = "debug")]
    {
        // SAFETY: この状態が元のグローバル環境で実行されることはありません。呼び出し側は
        // スクリプトを受け付ける前に独立した許可リスト付きの _ENV を構築し、debugテーブルはホスト専用にとどまります。
        Ok(unsafe { Lua::unsafe_new_with(libraries | StdLib::DEBUG, LuaOptions::default()) })
    }
    #[cfg(not(feature = "debug"))]
    {
        Lua::new_with(libraries, LuaOptions::default())
    }
}

fn thread_state(lua: &Lua, thread: &Thread) -> mlua::Result<NonNull<ffi::lua_State>> {
    let mut pointer = std::ptr::null_mut();
    // SAFETY: ルート化された1つのThread引数がこの一時スタック上に存在します。lua_tothread
    // はその引数を読み取ります。それをpopすることで期待される空の復帰スタックが復元されます。
    unsafe {
        lua.exec_raw::<()>(thread.clone(), |state| {
            pointer = ffi::lua_tothread(state, 1);
            ffi::lua_pop(state, 1);
        })?;
    }
    NonNull::new(pointer).ok_or_else(|| mlua::Error::RuntimeError("missing coroutine state".into()))
}

/// 停止したコルーチンの1フレーム。Cフレームは行番号を持ちません。
pub(crate) struct RawFrame {
    pub(crate) source: String,
    pub(crate) line: i32,
    pub(crate) name: Option<String>,
    pub(crate) is_lua: bool,
}

/// 実行していないコルーチンの呼び出し履歴を、スクリプトを実行せずに読み取ります。
/// エラーで終了したコルーチンと、yieldで中断したコルーチンの両方が対象です。
///
/// `hook_yielded`は、フックの中からyieldして止まったコルーチンに対して指定します。
/// その先頭フレームの行は照会せず-1を返すため、呼び出し側はフックで記録した行を使います。
pub(crate) fn thread_frames(
    lua: &Lua,
    thread: &Thread,
    limit: usize,
    hook_yielded: bool,
) -> mlua::Result<Vec<RawFrame>> {
    let state = thread_state(lua, thread)?;
    let mut frames = Vec::new();
    // SAFETY: `thread`がコルーチンを生存させ、呼び出し側はそのコルーチンを実行していません。
    // Lua 5.3はエラー終了後もCallInfoの連鎖と各フレームの関数スロットを巻き戻さないため、
    // lua_getstackは有効な活性レコードを返します。lua_getinfoの"Sln"は関数や表をpushせず、
    // メモリ確保もLuaエラーの送出もしません。返るCの文字列はProtoまたは静的領域が所有し、
    // このブロック内でコピーします。lua_Debugはゼロ初期化した値をLuaが埋めます。
    // フックからyieldしたフレームはLuaがsavedpcを1つ巻き戻しており、関数の最初の命令で
    // 止まった場合に"l"が行情報配列の手前を読むため、そのフレームには"l"を指定しません。
    unsafe {
        let state = state.as_ptr();
        for level in 0..limit {
            let mut record = std::mem::MaybeUninit::<ffi::lua_Debug>::zeroed();
            if ffi::lua_getstack(state, level as std::ffi::c_int, record.as_mut_ptr()) == 0 {
                break;
            }
            let skip_line = hook_yielded && level == 0;
            let what = if skip_line { c"Sn" } else { c"Sln" };
            if ffi::lua_getinfo(state, what.as_ptr(), record.as_mut_ptr()) == 0 {
                return Err(mlua::Error::RuntimeError("stack query failed".into()));
            }
            let record = record.assume_init();
            let text = |pointer: *const std::ffi::c_char| -> mlua::Result<Option<String>> {
                if pointer.is_null() {
                    return Ok(None);
                }
                std::ffi::CStr::from_ptr(pointer)
                    .to_str()
                    .map(|text| Some(text.to_owned()))
                    .map_err(|_| mlua::Error::RuntimeError("stack text is not UTF-8".into()))
            };
            frames.push(RawFrame {
                source: text(record.source)?.unwrap_or_default(),
                line: if skip_line { -1 } else { record.currentline },
                // 関数名はLuaの文字列（バイト列）に由来します。UTF-8でない名前は不明として扱い、
                // 位置情報全体を捨てません。
                name: text(record.name).ok().flatten(),
                is_lua: text(record.what)?.as_deref() != Some("C"),
            });
        }
    }
    Ok(frames)
}

pub(crate) struct YieldProbe {
    _thread: Thread,
    state: NonNull<ffi::lua_State>,
}
impl YieldProbe {
    pub(crate) fn new(lua: &Lua, thread: &Thread) -> mlua::Result<Self> {
        Ok(Self {
            _thread: thread.clone(),
            state: thread_state(lua, thread)?,
        })
    }
    #[cfg(feature = "debug")]
    pub(crate) fn local_names(&self) -> mlua::Result<Vec<(i32, Vec<u8>)>> {
        // SAFETY: 生きているコルーチンのフックから、Luaがyieldのためにsavedpcを巻き戻す前にのみ照会されます。
        // Cのデバッグ照会はスクリプトを実行しません。スタックスロットを1つ確保し、
        // ルート化された各ローカル変数名をコピーし、lua_getlocalによってpushされた値を正確にpopします。
        // これにより、yield後のgetlocalが一時変数としてラベル付けする前に、束縛の識別子を捕捉します。
        unsafe {
            let state = self.state.as_ptr();
            if ffi::lua_checkstack(state, 1) == 0 {
                return Err(mlua::Error::RuntimeError(
                    "debug stack capacity exceeded".into(),
                ));
            }
            let mut record = std::mem::MaybeUninit::<ffi::lua_Debug>::zeroed();
            if ffi::lua_getstack(state, 0, record.as_mut_ptr()) == 0 {
                return Ok(Vec::new());
            }
            let mut names = Vec::new();
            for index in 1..=1024 {
                let name = ffi::lua_getlocal(state, record.as_ptr(), index);
                if name.is_null() {
                    break;
                }
                names.push((index, std::ffi::CStr::from_ptr(name).to_bytes().to_vec()));
                ffi::lua_pop(state, 1);
            }
            Ok(names)
        }
    }
    pub(crate) fn can_yield(&self) -> bool {
        // SAFETY: mluaがVMロックを保持している間、このコルーチンのフック内でのみ呼び出されます。
        // ルート化されたThreadがコルーチンを生存させます。コルーチンもそのLua状態もSendではありません。
        // これは読み取り専用のC照会であり、Rustフレームをまたぐyield/longjmpではありません。
        unsafe { ffi::lua_isyieldable(self.state.as_ptr()) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(lua: &Lua, thread: &Thread, hook_yielded: bool) -> mlua::Result<Vec<(String, i32)>> {
        Ok(thread_frames(lua, thread, 256, hook_yielded)?
            .into_iter()
            .map(|frame| (frame.source, frame.line))
            .collect())
    }

    #[test]
    fn frames_survive_an_error_and_are_empty_before_start_and_after_return() -> mlua::Result<()> {
        let lua = Lua::new();
        let function = lua
            .load("local function f()\n local t\n return t.x\nend\nf()")
            .set_name("@unit.lua")
            .into_function()?;
        let failed = lua.create_thread(function)?;
        // 未開始のコルーチンは、開始する関数の準備フレームだけを持ちます。
        assert!(lines(&lua, &failed, false)?.len() <= 1);
        assert!(failed.resume::<()>(()).is_err());
        assert_eq!(
            lines(&lua, &failed, false)?,
            vec![("@unit.lua".to_owned(), 3), ("@unit.lua".to_owned(), 5)]
        );
        // 繰り返し読んでもスタックを変更しません。
        assert_eq!(lines(&lua, &failed, false)?.len(), 2);
        let finished = lua.create_thread(lua.load("return 1").into_function()?)?;
        finished.resume::<i32>(())?;
        assert!(lines(&lua, &finished, false)?.is_empty());
        Ok(())
    }

    #[test]
    fn hook_yield_on_the_first_instruction_does_not_query_the_top_line() -> mlua::Result<()> {
        let lua = Lua::new();
        let function = lua
            .load("local function spin()\n while true do end\nend\nspin()")
            .set_name("@unit.lua")
            .into_function()?;
        let thread = lua.create_thread(function)?;
        let count = std::cell::Cell::new(0);
        thread.set_hook(
            mlua::HookTriggers::new().every_nth_instruction(1),
            move |_, _| {
                count.set(count.get() + 1);
                // spinの最初の命令（ループのジャンプ）に入ってからyieldします。
                Ok(if count.get() > 8 {
                    mlua::VmState::Yield
                } else {
                    mlua::VmState::Continue
                })
            },
        );
        thread.resume::<()>(())?;
        assert_eq!(thread.status(), mlua::ThreadStatus::Resumable);
        assert_eq!(
            lines(&lua, &thread, true)?,
            vec![("@unit.lua".to_owned(), -1), ("@unit.lua".to_owned(), 4)]
        );
        Ok(())
    }

    #[test]
    fn frame_limit_bounds_deep_recursion() -> mlua::Result<()> {
        let lua = Lua::new();
        let function = lua
            .load("local function f(n) if n == 0 then error('deep') end return 1 + f(n - 1) end f(1000)")
            .into_function()?;
        let thread = lua.create_thread(function)?;
        assert!(thread.resume::<()>(()).is_err());
        assert_eq!(thread_frames(&lua, &thread, 256, false)?.len(), 256);
        Ok(())
    }
}
