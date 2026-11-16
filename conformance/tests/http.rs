//! 両プロファイルにおけるホストHTTPの識別性、順序付け、およびコールバック契約。
#[path = "support/assertions.rs"]
mod assertion_support;
use std::error::Error;
use storm_lua_addon::Addon;
use storm_lua_microcontroller::Microcontroller;
use storm_lua_vm::runner::{RunOutcome, StepMode};
#[test]
fn addon_reply_is_once_only_and_reload_invalidates_old_generation() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(&assertion_support::with_assertions(br#"g_savedata={count=0}
function onCreate() server.httpGet(8080,'/hello') end
function httpReply(port,request,reply) assert(port==8080 and request=='/hello'); g_savedata.count=g_savedata.count+1;debug.log(reply) end"#),"=http")?;
    addon.start()?;
    let request = addon.drain_http_requests().pop().ok_or("missing request")?;
    assert!(addon.drain_http_requests().is_empty());
    addon.http_reply(request.token, b"raw\0\xff")?;
    assert_eq!(addon.drain_log_records()[0].bytes, b"raw\0\xff");
    assert!(addon.http_reply(request.token, b"duplicate").is_err());
    let snapshot = addon.savedata()?;
    addon.reload(snapshot)?;
    addon.start()?;
    let next = addon
        .drain_http_requests()
        .pop()
        .ok_or("missing reloaded request")?;
    assert_ne!(request.token.generation, next.token.generation);
    assert!(addon.http_reply(request.token, b"stale").is_err());
    addon.cancel_http(next.token)?;
    assert!(addon.http_reply(next.token, b"cancelled").is_err());
    Ok(())
}
#[test]
fn vehicle_reply_waits_for_idle_and_keeps_the_token_when_busy() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        &assertion_support::with_assertions(
            b"async.httpGet(9000,'/status')
function onTick()
local a=1
a=a+1
end
function httpReply(p,q,r) debug.log(r) end",
        ),
        "=vehicle",
    )?;
    let request = vm.drain_http_requests().pop().ok_or("request")?;
    vm.set_breakpoints(vec![("=vehicle".into(), 4)])?;
    assert_eq!(vm.tick(&Default::default())?, RunOutcome::Suspended);
    assert!(vm.http_reply(request.token, b"first").is_err());
    vm.set_breakpoints(vec![])?;
    vm.resume(StepMode::Continue)?;
    vm.http_reply(request.token, b"second")?;
    assert_eq!(vm.drain_logs(), vec![b"second".to_vec()]);
    Ok(())
}
#[test]
fn requests_pass_through_without_game_absent_validation_and_foreign_tokens_reject(
) -> Result<(), Box<dyn Error>> {
    use storm_lua_spec::http::HttpQueue;
    let mut q = HttpQueue::new()?;
    // 実ゲームにない拒否はしない: 先頭が / でない・// 始まり・CR/LF・NUL混入・空・4096byte超も通す。
    for path in [
        b"//evil".as_slice(),
        b"http://evil",
        b"/a\r\nb",
        b"/a\0b",
        b"",
        b"relative",
        &[b'x'; 5000][..],
    ] {
        q.request(80, path)?;
    }
    // ポートは符号付き整数の下位16bitへ切り捨てる。0も負数も拒否しない。
    q.request(0, b"/zero")?;
    q.request(-1, b"/negative")?;
    q.request(70000, b"/wrapped")?;
    // 件数の上限も設けない。
    for _ in 0..256 {
        q.request(80, b"/ok")?;
    }
    let requests = q.drain();
    assert_eq!(requests.len(), 7 + 3 + 256);
    assert_eq!(requests[0].request, b"//evil");
    assert_eq!(requests[3].request, b"/a\0b");
    assert_eq!(requests[5].request, b"relative");
    assert_eq!(requests[6].request.len(), 5000);
    assert_eq!(requests[7].port, 0);
    assert_eq!(requests[8].port, u16::MAX);
    assert_eq!(requests[9].port, 4464);
    let mut other = HttpQueue::new()?;
    assert!(other.reply(requests[0].token, b"foreign").is_err());
    q.cancel(requests[0].token)?;
    q.request(80, b"/replacement")?;
    Ok(())
}

#[test]
fn large_reply_is_accepted_and_consumes_the_request() -> Result<(), Box<dyn std::error::Error>> {
    let mut queue = storm_lua_spec::http::HttpQueue::new()?;
    queue.request(8080, b"/status")?;
    let request = queue.drain().pop().ok_or("request")?;
    // 応答サイズの上限はない。大きな応答でも精算でき、トークンは消費される。
    let resolved = queue.reply(request.token, &vec![0; 2 * 1024 * 1024])?;
    assert_eq!(resolved, request);
    assert!(queue.reply(request.token, b"duplicate").is_err());
    Ok(())
}

#[test]
fn http_get_never_rejects_what_the_game_accepts() -> Result<(), Box<dyn Error>> {
    // Addon: server.httpGet はポート・リクエストの形でエラーを投げない。
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        &assertion_support::with_assertions(
            br#"function onCreate()
  server.httpGet(0, '')
  server.httpGet(-1, 'relative')
  server.httpGet(70000, '//double')
  server.httpGet(80, '/with\0nul')
end"#,
        ),
        "=http-validation",
    )?;
    addon.start()?;
    let requests = addon.drain_http_requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].port, 0);
    assert_eq!(requests[0].request, b"");
    assert_eq!(requests[1].port, u16::MAX);
    assert_eq!(requests[1].request, b"relative");
    assert_eq!(requests[2].port, 4464);
    assert_eq!(requests[3].request, b"/with\0nul");
    // Microcontroller: async.httpGet も同じ契約。
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        &assertion_support::with_assertions(
            b"async.httpGet(0, '')\nasync.httpGet(-1, 'relative')",
        ),
        "=mc-http-validation",
    )?;
    let requests = vm.drain_http_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].port, 0);
    assert_eq!(requests[1].port, u16::MAX);
    Ok(())
}
