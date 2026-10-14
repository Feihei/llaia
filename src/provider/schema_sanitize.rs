//! 工具参数 schema 清洗：发送给 OpenAI 兼容端点前把 `$ref` 递归内联。
//!
//! 背景：llama.cpp 的 json-schema-to-grammar 只在 schema **根部**找 `$defs`；
//! openCadStudio 等 MCP server 把 `$defs` 嵌在 anyOf 分支内部（`$ref` 指向
//! `#/$defs/<name>` 但根上没有该表），llama.cpp 编译 grammar 时报
//! "cannot resolve $ref, $defs not found"，**整个 chat 请求 500**（请求级失败，
//! 与模型打算调哪个工具无关）。云端 API 对两种写法都接受，内联是语义等价变换。
//!
//! 解析规则（宽松解析器语义）：`"#/$defs/<name>"` 优先按 JSON Pointer 从文档根
//! 查找；找不到时从 `$ref` 所在位置**向上**收集祖先链上的 `$defs`（最近优先）。
//! 循环引用在展开链上检测到后以空 schema `{}`（接受任意值）替换并 warn；
//! 存在无法解析的 `$ref` 时**原样返回整份 schema**（语义交给能解析的端点），
//! 不做半吊子改写。输出超过 [`MAX_OUTPUT_BYTES`] 同样回退原样。
//!
//! 只接入 openai_compatible 原生 tool calling 出站路径（grammar 编译只发生在
//! 这一族端点）；anthropic / gemini / 标签协议路径 schema 原样透传。

use serde_json::{Map, Value};
use tracing::warn;

/// 递归展开深度上限（正常 schema 远达不到；防御循环/病态嵌套）。
const MAX_DEPTH: usize = 64;
/// 内联产物体积上限：超限说明共享 def 被大量复制，回退原样发送。
const MAX_OUTPUT_BYTES: usize = 1_000_000;

/// 返回内联后的 schema。保证不 panic、不返回 `Null`。
pub fn sanitize_tool_schema(params: &Value) -> Value {
    let root = params;
    let stack: Vec<&Map<String, Value>> = Vec::new();
    let mut ctx = InlineCtx {
        chain: Vec::new(),
        unresolved: false,
        changed: false,
    };
    let out = inline(root, &stack, 0, root, &mut ctx);
    if ctx.unresolved {
        // 有解析不了的 ref：动一半比不动更糟，整份回退原样。
        warn!("[schema-sanitize] unresolved $ref remains; sending schema as-is");
        return params.clone();
    }
    if !ctx.changed {
        return params.clone();
    }
    let size = serde_json::to_string(&out).map(|s| s.len()).unwrap_or(0);
    if size > MAX_OUTPUT_BYTES {
        warn!(
            size,
            cap = MAX_OUTPUT_BYTES,
            "[schema-sanitize] inlined schema exceeds size cap; sending schema as-is"
        );
        return params.clone();
    }
    out
}

struct InlineCtx {
    /// 当前展开中的 `$ref` 指针链（循环检测）。
    chain: Vec<String>,
    unresolved: bool,
    changed: bool,
}

/// 从 `$ref` 所在位置解析目标值。先按 JSON Pointer 从根查；失败则沿祖先
/// `$defs` 链（最近优先）查 `#/$defs/<name>` 形态。
fn resolve_ref<'a>(
    pointer: &str,
    stack: &[&'a Map<String, Value>],
    root: &'a Value,
) -> Option<&'a Value> {
    let target = pointer.strip_prefix('#').unwrap_or(pointer);
    if let Some(v) = root.pointer(target) {
        return Some(v);
    }
    let name = target.strip_prefix("/$defs/")?;
    let name = unescape_pointer_token(name);
    for defs in stack.iter().rev() {
        if let Some(v) = defs.get(&name) {
            return Some(v);
        }
    }
    None
}

fn unescape_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn inline(
    v: &Value,
    stack: &[&Map<String, Value>],
    depth: usize,
    root: &Value,
    ctx: &mut InlineCtx,
) -> Value {
    if depth > MAX_DEPTH {
        warn!(
            "[schema-sanitize] expansion depth cap hit; replacing subtree with permissive schema"
        );
        ctx.changed = true;
        return Value::Object(Map::new());
    }
    match v {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get("$ref") {
                // 展开链上出现同一指针 = 循环引用，以空 schema 截断。
                if ctx.chain.iter().any(|p| p == r) {
                    warn!(ref = %r, "[schema-sanitize] circular $ref; replacing with permissive schema");
                    ctx.changed = true;
                    return Value::Object(Map::new());
                }
                return match resolve_ref(r, stack, root) {
                    Some(target) => {
                        ctx.changed = true;
                        ctx.chain.push(r.clone());
                        // 目标自身可能带 `$defs`（局部表），压栈供其内部 ref 就近解析。
                        let mut sub_stack: Vec<&Map<String, Value>> = stack.to_vec();
                        if let Some(defs) = target.get("$defs").and_then(Value::as_object) {
                            sub_stack.push(defs);
                        }
                        let out = inline(target, &sub_stack, depth + 1, root, ctx);
                        ctx.chain.pop();
                        out
                    }
                    None => {
                        ctx.unresolved = true;
                        return v.clone();
                    }
                };
            }
            let own_defs = map.get("$defs").and_then(Value::as_object);
            let mut new_stack: Vec<&Map<String, Value>> = stack.to_vec();
            if let Some(defs) = own_defs {
                new_stack.push(defs);
            }
            let mut out = Map::with_capacity(map.len());
            for (k, val) in map {
                if k == "$defs" {
                    // 已全部内联，剥离局部表（无 ref 残留时才会走到这）。
                    ctx.changed = true;
                    continue;
                }
                out.insert(k.clone(), inline(val, &new_stack, depth, root, ctx));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| inline(x, stack, depth, root, ctx))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn passthrough_without_refs() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a"]
        });
        assert_eq!(sanitize_tool_schema(&schema), schema);
    }

    #[test]
    fn inlines_root_defs() {
        let schema = json!({
            "type": "object",
            "properties": {"h": {"$ref": "#/$defs/handle"}},
            "$defs": {"handle": {"type": "string"}}
        });
        let out = sanitize_tool_schema(&schema);
        assert!(out.to_string().contains("\"type\":\"string\""));
        assert!(!out.to_string().contains("$ref"));
        assert!(!out.to_string().contains("$defs"));
    }

    /// openCadStudio 形状：`$defs` 嵌在 anyOf 分支内部，根部没有。
    #[test]
    fn inlines_nested_defs_inside_anyof_branch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "request": {
                    "anyOf": [
                        {"type": "object", "properties": {"op": {"type": "string"}}},
                        {"type": "object",
                         "properties": {"handle": {"$ref": "#/$defs/handle"}},
                         "$defs": {"handle": {"type": "string"}}}
                    ]
                }
            }
        });
        let out = sanitize_tool_schema(&schema);
        let s = out.to_string();
        assert!(!s.contains("$ref"), "residual ref: {s}");
        assert!(!s.contains("$defs"), "residual defs: {s}");
        assert!(s.contains("\"type\":\"string\""));
    }

    /// 同名 def 在不同分支各自定义：就地解析必须命中各自分支的版本。
    #[test]
    fn nearest_defs_wins_per_branch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "request": {
                    "anyOf": [
                        {"type": "object",
                         "properties": {"handle": {"$ref": "#/$defs/handle"}},
                         "$defs": {"handle": {"type": "string"}}},
                        {"type": "object",
                         "properties": {"handle": {"$ref": "#/$defs/handle"}},
                         "$defs": {"handle": {"type": "integer"}}}
                    ]
                }
            }
        });
        let out = sanitize_tool_schema(&schema);
        let s = out.to_string();
        assert!(s.contains("\"type\":\"string\""));
        assert!(s.contains("\"type\":\"integer\""));
        assert!(!s.contains("$ref"));
    }

    /// 分支没有局部 defs 时，向上命中根部的同名 def。
    #[test]
    fn falls_back_to_root_defs() {
        let schema = json!({
            "type": "object",
            "properties": {
                "request": {
                    "anyOf": [
                        {"type": "object",
                         "properties": {"handle": {"$ref": "#/$defs/handle"}}}
                    ]
                }
            },
            "$defs": {"handle": {"type": "string"}}
        });
        let out = sanitize_tool_schema(&schema);
        let s = out.to_string();
        assert!(s.contains("\"type\":\"string\""));
        assert!(!s.contains("$ref"));
    }

    #[test]
    fn circular_ref_replaced_with_permissive_schema() {
        let schema = json!({
            "type": "object",
            "properties": {"node": {"$ref": "#/$defs/node"}},
            "$defs": {"node": {
                "type": "object",
                "properties": {"child": {"$ref": "#/$defs/node"}}
            }}
        });
        let out = sanitize_tool_schema(&schema);
        let s = out.to_string();
        assert!(!s.contains("$ref"));
        assert!(s.contains("\"child\":{}"), "cycle site: {s}");
    }

    #[test]
    fn unresolved_ref_returns_original() {
        let schema = json!({
            "type": "object",
            "properties": {"h": {"$ref": "#/$defs/missing"}}
        });
        assert_eq!(sanitize_tool_schema(&schema), schema);
    }

    /// def 之间的引用（同一局部表内互相 $ref）也要递归展开。
    #[test]
    fn def_to_def_refs_resolved() {
        let schema = json!({
            "type": "object",
            "properties": {"w": {"$ref": "#/$defs/widget"}},
            "$defs": {
                "widget": {
                    "type": "object",
                    "properties": {"label": {"$ref": "#/$defs/label"}}
                },
                "label": {"type": "string"}
            }
        });
        let out = sanitize_tool_schema(&schema);
        let s = out.to_string();
        assert!(!s.contains("$ref"));
        assert!(s.contains("\"type\":\"string\""));
    }

    #[test]
    fn pointer_escapes_handled() {
        let schema = json!({
            "type": "object",
            "properties": {"a/b~c": {"$ref": "#/properties/a~1b~0c"}}
        });
        let out = sanitize_tool_schema(&schema);
        assert!(!out.to_string().contains("$ref"));
    }
}
