import json
cases=list(map(json.loads,open(__import__('pathlib').Path(__file__).with_name('cases.jsonl'))))
cases += [dict(name='unicode-basic',source='-- 雪😀\nlocal text="雪😀" function onDraw()screen.drawText(0,0,text)end',skip_eval=True),dict(name='extended-lexical',source='local s="\\255"; function onDraw() screen.drawText(0,0,s) end',skip_eval=True,options={'environment':'extended'}),dict(name='env-lexical',source='local _ENV=_ENV; function onDraw()screen.drawText(0,0,"hello")end',skip_eval=True)]
configs=[{}, {'searchMode':'fast','searchBeamWidth':1},{'searchMode':'fast','searchBeamWidth':16},{'targetSize':0},{'targetSize':8192},{'zeroCostNewlines':False},{'numericMode':'exact','mode':'safe'}]
for case in cases:
 for i,cfg in enumerate(configs):
  for m in [False,True]:
   c=case.copy();c['name']+=f'-config{i}-map{m}';c['options']={**c.get('options',{}),**cfg,'sourceMap':m,'sourceName':'controller雪😀.lua'};c['skip_eval']=True;print(json.dumps(c))
