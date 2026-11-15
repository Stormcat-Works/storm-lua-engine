// cargo xtask generate によって自動生成されています。編集しないでください。
export const ABI = {
  "abiVersion": 1,
  "capabilities": 31,
  "channelCount": 32,
  "inputBooleansOffset": 128,
  "inputNumbersOffset": 0,
  "ioAlignment": 4,
  "ioByteLength": 320,
  "numberBytes": 4,
  "outputBooleansOffset": 288,
  "outputNumbersOffset": 160
} as const;
/** EngineError.code と生のC ABIが返す結果コード。 */
export const STATUS = {
  "busy": 5,
  "failed": 9,
  "host": 10,
  "invalidArgument": 4,
  "invalidHandle": 3,
  "limit": 2,
  "lua": 1,
  "missing": 8,
  "ok": 0,
  "suspended": 7,
  "unsupported": 6
} as const;
