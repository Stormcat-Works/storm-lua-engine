//! 検証済みのWASMアダプタ。生ポインタは所有された安定したバッファに対してのみ返されます。
use storm_lua_bridge::{self as bridge, BridgeError, Status};
mod codec;
#[cfg(feature = "debug")]
mod debug_adapter;
mod host;
mod script;
mod services;
mod session;
mod value_codec;
bridge::memory_exports!(
    sle_alloc,
    sle_dealloc,
    sle_error_status,
    sle_error_ptr,
    sle_error_len,
    sle_response_ptr,
    sle_response_len
);
/// 現在の固定I/OおよびコマンドABIリビジョン。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_abi_version() -> u32 {
    storm_lua_spec::abi::ABI_VERSION
}

/// このモジュールビルドで実際に利用可能な機能（ケイパビリティ）。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_capabilities() -> u32 {
    storm_lua_spec::abi::CAP_RASTER
        | storm_lua_spec::abi::CAP_RUNTIME
        | storm_lua_spec::abi::CAP_ADDON
        | if cfg!(target_os = "emscripten") {
            storm_lua_spec::abi::CAP_HOST_SERVICES
        } else {
            0
        }
        | if cfg!(feature = "debug") {
            storm_lua_spec::abi::CAP_DEBUG
        } else {
            0
        }
}

/// バンドルされた印字可能ASCIIの1行（row）を読み取ります。無効なインデックスは -1 を返します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_font_row(codepoint: u32, row: u32) -> i32 {
    storm_screen_raster::font::ascii_glyph(codepoint)
        .and_then(|g| g.get(row as usize))
        .map_or(-1, |v| i32::from(*v))
}

/// 世代付きハンドルを破棄します。重複破棄はエラーとなります。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_dispose(handle: u32) -> i32 {
    bridge::call(|| session::dispose(handle))
}

/// 明示的なリソースバジェットを指定して、未開始のコントローラを生成します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_new(instruction_budget: u32, memory_bytes: u32) -> u32 {
    bridge::value(|| session::create(instruction_budget, memory_bytes)) as u32
}

/// 2つの登録済みアップロードバッファからソースをロードして実行します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_load(
    handle: u32,
    source_ptr: usize,
    source_len: u32,
    name_ptr: usize,
    name_len: u32,
) -> i32 {
    bridge::call(|| {
        bridge::with_upload(source_ptr, source_len, |source| {
            bridge::with_upload(name_ptr, name_len, |name| {
                let name = std::str::from_utf8(name).map_err(|_| {
                    BridgeError::new(Status::InvalidArgument, "chunk name is not UTF-8")
                })?;
                if name.len() > 1024 {
                    return Err(BridgeError::new(
                        Status::InvalidArgument,
                        "chunk name is too long",
                    ));
                }
                session::with(handle, |session| {
                    session.idle()?;
                    let result = session.vm.load(source, name);
                    session.sync_output();
                    result.map(session::outcome).map_err(session::convert)
                })
            })
        })
    })
}

/// 固定I/Oブロックを使用して1 tick を実行します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_tick(handle: u32) -> i32 {
    bridge::call(|| session::with(handle, |s| s.tick()))
}

/// onDraw を実行し、その現在のコマンドプレフィックスを正確にラスタライズします。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_draw(handle: u32, width: u32, height: u32) -> i32 {
    bridge::call(|| session::with(handle, |s| s.draw(width, height)))
}

/// 保持されているソースと現在のプロパティを使用してLuaステートを再作成します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_reset(handle: u32) -> i32 {
    bridge::call(|| session::with(handle, |s| s.reset()))
}

/// 安定した320バイトのI/Oブロックを参照します。Rust bool の再解釈は行いません。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_io_ptr(handle: u32) -> usize {
    bridge::value(|| session::with(handle, |s| s.io_ptr()))
}

/// 検証済みのロスレスなコールドパス要求からプロパティをアトミックに差し替えます。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_set_properties(handle: u32, pointer: usize, length: u32) -> i32 {
    bridge::call(|| {
        bridge::with_upload(pointer, length, |bytes| {
            let properties = codec::properties(bytes)?;
            session::with(handle, |s| {
                s.vm.vehicle()?
                    .set_properties(properties)
                    .map_err(session::convert)?;
                Ok(Status::Ok)
            })
        })
    })
}

/// 生のdebugアクセスではなく、開発用の print/debug.log を明示的に有効化します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_enable_logs(handle: u32) -> i32 {
    bridge::call(|| {
        session::with(handle, |s| {
            s.vm.enable_dev_logs().map_err(session::convert)?;
            Ok(Status::Ok)
        })
    })
}

/// 生のログバイト配列をコールドレスポンスとして返します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_drain_logs(handle: u32) -> i32 {
    bridge::call(|| services::logs(handle, false))
}

/// 型付きホストデバッガ要求を実行します。未サポートのビルドでは明示的に報告されます。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_debug(handle: u32, pointer: usize, length: u32) -> i32 {
    bridge::call(|| {
        #[cfg(feature = "debug")]
        {
            bridge::with_upload(pointer, length, |bytes| {
                debug_adapter::request(handle, bytes)
            })
        }
        #[cfg(not(feature = "debug"))]
        {
            let _ = (handle, pointer, length);
            Err(BridgeError::new(
                Status::Unsupported,
                "debugger was disabled for this build",
            ))
        }
    })
}

/// フレームの変更または破棄が発生するまで、生のRGBAバイト列を参照します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_frame_ptr(handle: u32) -> usize {
    bridge::value(|| {
        session::with(handle, |s| {
            let raster = s.raster.as_ref().ok_or_else(|| {
                BridgeError::new(Status::InvalidArgument, "no frame has been drawn")
            })?;
            Ok(raster.pixels().as_ptr() as usize)
        })
    })
}

/// フレームのバイト長。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_frame_len(handle: u32) -> usize {
    bridge::value(|| {
        session::with(handle, |s| {
            let raster = s.raster.as_ref().ok_or_else(|| {
                BridgeError::new(Status::InvalidArgument, "no frame has been drawn")
            })?;
            Ok(raster.pixels().len())
        })
    })
}

/// 現在のフレーム幅。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_frame_width(handle: u32) -> usize {
    bridge::value(|| {
        session::with(handle, |s| {
            let raster = s.raster.as_ref().ok_or_else(|| {
                BridgeError::new(Status::InvalidArgument, "no frame has been drawn")
            })?;
            Ok(raster.dimensions().0 as usize)
        })
    })
}

/// 現在のフレーム高さ。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_frame_height(handle: u32) -> usize {
    bridge::value(|| {
        session::with(handle, |s| {
            let raster = s.raster.as_ref().ok_or_else(|| {
                BridgeError::new(Status::InvalidArgument, "no frame has been drawn")
            })?;
            Ok(raster.dimensions().1 as usize)
        })
    })
}

/// 現在のフレーム世代（epoch）。変更後に古い参照（lease）を使用してはなりません。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_frame_epoch(handle: u32) -> u32 {
    bridge::value(|| session::with(handle, |s| Ok(s.epoch as usize))) as u32
}

/// 明示的な new-world/save/server プロバイダ設定を使用してアドオンを生成します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_new_addon(
    instructions: u32,
    memory: u32,
    host_key: u32,
    pointer: usize,
    length: u32,
) -> u32 {
    bridge::value(|| {
        bridge::with_upload(pointer, length, |bytes| {
            services::create_addon(instructions, memory, host_key, bytes)
        })
    }) as u32
}
/// モードチェックされたアドオンのライフサイクル、イベント、またはチェックポイント操作をディスパッチします。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_addon(handle: u32, pointer: usize, length: u32) -> i32 {
    bridge::call(|| bridge::with_upload(pointer, length, |bytes| services::addon(handle, bytes)))
}
/// HTTPリクエストの取り出し、返答、またはキャンセルを行います。ネットワークI/Oは決して実行しません。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_http(handle: u32, pointer: usize, length: u32) -> i32 {
    bridge::call(|| bridge::with_upload(pointer, length, |bytes| services::http(handle, bytes)))
}
/// print/debug.log の出力元付きでログを取得します。生のバイト列は保持されます。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_drain_log_records(handle: u32) -> i32 {
    bridge::call(|| services::logs(handle, true))
}
/// ビークル用の登録済みJSマッププロバイダを選択します。0を指定すると明示的に解除されます。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_set_map_host(handle: u32, host_key: u32) -> i32 {
    bridge::call(|| session::with(handle, |s| s.set_map_host(host_key)))
}
/// プロファイル種別を照会します: 1=vehicle, 2=addon。0は無効なハンドルを示します。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_mode(handle: u32) -> u32 {
    bridge::value(|| {
        session::with(handle, |s| {
            Ok(match s.vm {
                script::Script::Vehicle(_) => 1,
                script::Script::Addon(_) => 2,
            })
        })
    }) as u32
}

/// Create a Vehicle with explicit environment, properties and trusted host binding paths.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_new_vehicle(
    instruction_budget: u32,
    memory_bytes: u32,
    host_key: u32,
    pointer: usize,
    length: u32,
) -> u32 {
    bridge::value(|| {
        bridge::with_upload(pointer, length, |bytes| {
            services::create_vehicle(instruction_budget, memory_bytes, host_key, bytes)
        })
    }) as u32
}

/// Execute a named Vehicle callback or inspect current typed properties. No source rewriting.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_vehicle(handle: u32, pointer: usize, length: u32) -> i32 {
    bridge::call(|| bridge::with_upload(pointer, length, |bytes| services::vehicle(handle, bytes)))
}
