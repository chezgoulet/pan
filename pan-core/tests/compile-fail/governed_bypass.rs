// EXPECT: E0451 (field `request` is private)
// Fabricating a Governed token would skip the govern stage. Must not compile.
use pan_core::pipeline::*;
fn main() {
    let req = EffectRequest { capability: "x".into(), args: serde_json::Value::Null, correlation: None };
    let _g = Governed { request: req };
}
