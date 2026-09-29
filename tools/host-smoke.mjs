/** ブラウザ非依存の結合シナリオ。Nodeおよび3つの主要ブラウザエンジンすべてで使用されます。 */
export function hostSmoke(engine, api) {
  const check = (condition,message) => { if (!condition) throw new Error(message); };
  const logs=[], calls=[];
  const addon=engine.createAddon({
    environment:'extended',
    properties:{Rate:2.5}, onLog:record=>logs.push(record),
    server:{
      getPlayers:()=>[{kind:'table',entries:[[1n,api.luaTable({id:7n,name:'tester'})]]}],
      echo:(...args)=>{calls.push(args);return [...args,null];}
    }
  });
  try {
    check(addon.mode==='addon' && !('io' in addon) && !('draw' in addon),'addon type isolation');
    addon.load(`assert(input==nil and output==nil and screen==nil and async==nil)
assert(property.getNumber==nil and debug.getregistry==nil)
g_savedata={ticks=0,rate=property.slider('Rate',0,10,0.5,1),raw=string.char(0,255),big=9223372036854775807}
function onCreate(new)
 assert(g_savedata.rate==2.5)
 local players=server.getPlayers();assert(players[1].id==7 and players[1].name=='tester')
 local r=table.pack(server.echo(9223372036854775807,string.char(0,255),nil))
 assert(r.n==4 and r[1]==9223372036854775807 and r[2]==string.char(0,255) and r[3]==nil and r[4]==nil)
 server.httpGet(8080,'/status');debug.log('created',new)
end
function onTick(dt) g_savedata.ticks=g_savedata.ticks+dt;print('ticks',dt) end
function onChatMessage(peer,name,message) g_savedata.message=message end
function httpReply(port,path,reply) assert(port==8080 and path=='/status');g_savedata.reply=reply end`,'=addon-smoke');
    addon.start();addon.tick(400);addon.dispatch('onChatMessage',[7n,'tester','hello']);
    check(calls[0][0]===9223372036854775807n && calls[0][1][1]===255 && calls[0][2]===null,'lossless host arguments');
    check(logs.length===2 && logs[0].source==='debug.log' && logs[1].source==='print','structured automatic logs');
    check(logs[0].location?.chunk==='=addon-smoke' && logs[0].location.line===9 && logs[1].location?.line===11,'log emission locations');
    const requests=addon.drainHttpRequests();check(requests.length===1,'HTTP request delivery');
    addon.httpReply(requests[0].token,new Uint8Array([0,255]));
    let duplicate=false;try {addon.httpReply(requests[0].token,'again');} catch(error) {duplicate=error.code===4;}
    check(duplicate,'duplicate HTTP reply rejection');
    const snapshot=api.decodeSavedata(api.encodeSavedata(addon.savedata()));
    check(api.luaField(snapshot,'ticks')===400n && api.luaField(snapshot,'big')===9223372036854775807n,'checkpoint integers');
    check(api.luaText(api.luaField(snapshot,'message'))==='hello' && api.luaField(snapshot,'reply')[1]===255,'checkpoint strings');
    addon.reload(snapshot);addon.start();addon.tick(1);
    check(api.luaField(addon.savedata(),'ticks')===401n,'savedata restore before onCreate');
    const next=addon.drainHttpRequests()[0];check(next.token.generation!==requests[0].token.generation,'HTTP generation on reload');addon.cancelHttp(next.token);
    let wrongMode=false;try {engine.bridge.call('tick',addon.handle);} catch(error) {wrongMode=error.code===4;}
    check(wrongMode,'raw ABI mode rejection');
    addon.destroy();
  } finally {addon.dispose();}
  const maps=[];
  const vehicle=engine.createVehicle({mapProvider:request=>{
    maps.push(request);const bytes=new Uint8Array(request.width*request.height*4);
    for(let i=0;i<bytes.length;i+=4)bytes.set([0,0,64,255],i);
    return bytes;
  }});
  try {
    vehicle.load('local function assert(v)if not v then local fail=nil;fail()end end;assert(server==nil and matrix==nil) function onDraw() screen.setColor(255,0,0);screen.setMapColorOcean(0,0,64);screen.drawMap(4,5,6);screen.drawRectF(0,0,1,1) end','=map-smoke');
    vehicle.draw(2,2);const pixels=vehicle.frame().copy();
    check(maps.length===1 && maps[0].center[0]===4 && maps[0].colors.ocean[2]===64 && maps[0].colors.land===undefined,'map request contract');
    check(pixels[0]===255 && pixels[4]===0 && pixels[6]===64,'map order and draw color');
  } finally {vehicle.dispose();}
  const controls=engine.createVehicle({environment:'extended',controlNamespace:'harness'});
  try {
    controls.load('harness.setProperty("gain",16777217);function check(v)harness.setInputNumber(1,v);output.setNumber(1,property.getNumber("gain")-16777216);output.setNumber(2,input.getNumber(1))end function paint()screen.setColor(255,0,0);screen.drawRectF(0,0,1,1)end','@controls.lua');
    controls.callTick('check',[16777217]);
    check(controls.io.outputNumbers[0]===1&&controls.io.outputNumbers[1]===16777216,'named callback and native state controls');
    controls.callDraw('paint',32,32);check(controls.frame().pixels[0]===255,'named draw');
    check(controls.properties()[0].value.value===16777217,'typed property snapshot');
  }finally{controls.dispose();}
  return {addon:true,hostServer:true,mapProvider:true,http:true,structuredLogs:true,logLocations:true};
}
