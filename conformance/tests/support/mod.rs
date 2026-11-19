use serde_json::Value;
use std::error::Error;
use std::fmt::Write as _;
use storm_lua_spec::{draw::DrawCommand, screen::Rgba8};
pub fn command(v: &Value) -> Result<DrawCommand, Box<dyn Error>> {
    let n = |key: &str| -> Result<f64, Box<dyn Error>> {
        v[key]
            .as_f64()
            .ok_or_else(|| format!("missing number {key}").into())
    };
    let text = || -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(v["text"]
            .as_str()
            .ok_or("missing text")?
            .as_bytes()
            .to_vec())
    };
    let kind = v["type"].as_str().ok_or("missing op type")?;
    Ok(match kind {
        "setColour" => DrawCommand::SetColor(Rgba8([
            n("r")?.round().clamp(0.0, 255.0) as u8,
            n("g")?.round().clamp(0.0, 255.0) as u8,
            n("b")?.round().clamp(0.0, 255.0) as u8,
            v["a"].as_f64().unwrap_or(255.0).round().clamp(0.0, 255.0) as u8,
        ])),
        "clear" => DrawCommand::Clear,
        "line" => DrawCommand::Line([[n("x1")?, n("y1")?], [n("x2")?, n("y2")?]]),
        "rect" | "rectF" => {
            DrawCommand::Rect([n("x")?, n("y")?, n("w")?, n("h")?], kind == "rectF")
        }
        "circle" | "circleF" => {
            DrawCommand::Circle([n("x")?, n("y")?, n("radius")?], kind == "circleF")
        }
        "triangle" | "triangleF" => DrawCommand::Triangle(
            [
                [n("x1")?, n("y1")?],
                [n("x2")?, n("y2")?],
                [n("x3")?, n("y3")?],
            ],
            kind == "triangleF",
        ),
        "text" => DrawCommand::Text([n("x")?, n("y")?], text()?),
        "textBox" => DrawCommand::TextBox(
            [
                n("x")?,
                n("y")?,
                n("w")?,
                n("h")?,
                n("horizontalAlign")?,
                n("verticalAlign")?,
            ],
            text()?,
        ),
        _ => return Err(format!("unknown op {kind}").into()),
    })
}

/// 採用済み入力をLuaのscreen APIへ変換します。期待画素は生成しません。
pub fn lua_source(ops: &[Value]) -> Result<String, Box<dyn Error>> {
    let mut source = String::from("function onDraw()\n");
    for op in ops {
        let kind = op["type"].as_str().ok_or("command type missing")?;
        let (name, fields): (&str, &[&str]) = match kind {
            "setColour" => ("setColor", &["r", "g", "b"]),
            "clear" => ("drawClear", &[]),
            "line" => ("drawLine", &["x1", "y1", "x2", "y2"]),
            "rect" => ("drawRect", &["x", "y", "w", "h"]),
            "rectF" => ("drawRectF", &["x", "y", "w", "h"]),
            "circle" => ("drawCircle", &["x", "y", "radius"]),
            "circleF" => ("drawCircleF", &["x", "y", "radius"]),
            "triangle" => ("drawTriangle", &["x1", "y1", "x2", "y2", "x3", "y3"]),
            "triangleF" => ("drawTriangleF", &["x1", "y1", "x2", "y2", "x3", "y3"]),
            "text" => ("drawText", &["x", "y"]),
            "textBox" => ("drawTextBox", &["x", "y", "w", "h"]),
            _ => return Err(format!("unknown drawing operation {kind}").into()),
        };
        let mut args = Vec::new();
        for field in fields {
            args.push(format!(
                "{:?}",
                op[*field].as_f64().ok_or("coordinate missing")?
            ));
        }
        if kind == "setColour" && op.get("a").is_some() {
            args.push(format!("{:?}", op["a"].as_f64().ok_or("invalid alpha")?));
        }
        if kind == "text" || kind == "textBox" {
            let bytes = op["text"].as_str().ok_or("text missing")?.as_bytes();
            let mut escaped = String::new();
            for byte in bytes {
                write!(escaped, "\\{byte:03}")?;
            }
            args.push(format!("\"{escaped}\""));
        }
        if kind == "textBox" {
            for field in ["horizontalAlign", "verticalAlign"] {
                args.push(format!(
                    "{:?}",
                    op[field].as_f64().ok_or("alignment missing")?
                ));
            }
        }
        writeln!(source, "screen.{name}({})", args.join(","))?;
    }
    source.push_str("end");
    Ok(source)
}
