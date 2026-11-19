//! インタプリタやオプティマイザに依存しないマイクロコントローラーAPIメタデータ。
//! Luaのグローバル変数が再束縛可能な場合、副作用（effect）の説明は呼び出しの純粋性を保証しません。

/// エンジンAPI契約の1項目。availability は本実装の状態を表し、未測定のゲーム挙動ではありません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiFunction {
    /// ドット区切りのLua名。
    pub path: &'static str,
    /// 名前付き引数。? は省略可能引数、... は可変長引数を表します。
    pub parameters: &'static str,
    /// 戻り値型のリスト。空文字列は戻り値なしを意味します。
    pub returns: &'static str,
    /// コールバックコンテキスト: any, tick, draw。
    pub phase: &'static str,
    /// implemented（実装済み）、host-extension（ホスト拡張）、または requires-provider（プロバイダ必要）。
    pub availability: &'static str,
    /// 副作用の説明カテゴリ（オプティマイザ向けの純粋性証明ではありません）。
    pub effect: &'static str,
}
/// エンジン提供のAPIのみ。Lua標準ライブラリはVM管理の独立した許可リストを持ちます。
pub const FUNCTIONS: &[ApiFunction] = &[
    ApiFunction { path: "screen.setMapColorOcean", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "screen.setMapColorShallows", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "screen.setMapColorLand", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "screen.setMapColorGrass", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "screen.setMapColorSand", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "screen.setMapColorSnow", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "screen.setMapColorRock", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "screen.setMapColorGravel", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "map-palette" },
    ApiFunction { path: "async.httpGet", parameters: "port: integer, request: string", returns: "", phase: "any", availability: "implemented", effect: "queue-http" },
    ApiFunction { path: "input.getNumber", parameters: "channel: integer", returns: "number", phase: "tick", availability: "implemented", effect: "read-signal" },
    ApiFunction { path: "input.getBool", parameters: "channel: integer", returns: "boolean", phase: "tick", availability: "implemented", effect: "read-signal" },
    ApiFunction { path: "output.setNumber", parameters: "channel: integer, value: number", returns: "", phase: "tick", availability: "implemented", effect: "write-signal" },
    ApiFunction { path: "output.setBool", parameters: "channel: integer, value: boolean", returns: "", phase: "tick", availability: "implemented", effect: "write-signal" },
    ApiFunction { path: "property.getNumber", parameters: "label: string", returns: "number", phase: "any", availability: "implemented", effect: "read-property" },
    ApiFunction { path: "property.getBool", parameters: "label: string", returns: "boolean", phase: "any", availability: "implemented", effect: "read-property" },
    ApiFunction { path: "property.getText", parameters: "label: string", returns: "string", phase: "any", availability: "implemented", effect: "read-property" },
    ApiFunction { path: "screen.getWidth", parameters: "", returns: "number", phase: "draw", availability: "implemented", effect: "read-context" },
    ApiFunction { path: "screen.getHeight", parameters: "", returns: "number", phase: "draw", availability: "implemented", effect: "read-context" },
    ApiFunction { path: "screen.setColor", parameters: "r: number, g: number, b: number, a?: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawClear", parameters: "", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawLine", parameters: "x1: number, y1: number, x2: number, y2: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawRect", parameters: "x: number, y: number, width: number, height: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawRectF", parameters: "x: number, y: number, width: number, height: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawCircle", parameters: "x: number, y: number, radius: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawCircleF", parameters: "x: number, y: number, radius: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawTriangle", parameters: "x1: number, y1: number, x2: number, y2: number, x3: number, y3: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawTriangleF", parameters: "x1: number, y1: number, x2: number, y2: number, x3: number, y3: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawText", parameters: "x: number, y: number, text: string", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawTextBox", parameters: "x: number, y: number, width: number, height: number, text: string, horizontalAlign?: number, verticalAlign?: number", returns: "", phase: "draw", availability: "implemented", effect: "draw-command" },
    ApiFunction { path: "screen.drawMap", parameters: "x: number, y: number, zoom: number", returns: "", phase: "draw", availability: "requires-provider", effect: "host-service" },
    ApiFunction { path: "map.screenToMap", parameters: "mapX: number, mapY: number, zoom: number, screenWidth: number, screenHeight: number, pixelX: number, pixelY: number", returns: "number, number", phase: "any", availability: "implemented", effect: "coordinate-conversion" },
    ApiFunction { path: "map.mapToScreen", parameters: "mapX: number, mapY: number, zoom: number, screenWidth: number, screenHeight: number, worldX: number, worldY: number", returns: "number, number", phase: "any", availability: "implemented", effect: "coordinate-conversion" },
    ApiFunction { path: "print", parameters: "...: any", returns: "", phase: "any", availability: "host-extension", effect: "log" },
    ApiFunction { path: "debug.log", parameters: "...: any", returns: "", phase: "any", availability: "implemented", effect: "log" },
];
