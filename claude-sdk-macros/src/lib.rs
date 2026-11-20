//! #[claude_tool] — turn a (async) fn into a Claude tool at compile time.
//!
//! Usage:
//! ```rust,ignore
//! /// Search the filesystem for files matching a pattern
//! #[claude_tool]
//! async fn search_files(pattern: String, max_results: u64) -> anyhow::Result<Vec<String>> {
//!     // implementation
//! }
//! ```
//!
//! Generates, next to the fn:
//! ```rust,ignore
//! pub fn search_files_tool_def() -> claude_sdk::protocol::ToolDef { ... }
//! pub fn search_files_tool() -> claude_sdk::SdkTool { ... } // def + handler
//! ```
//! Register the tool with `SdkMcpServer::new("name").tool(search_files_tool())`.
//!
//! Arguments are deserialized with serde (`T: DeserializeOwned`; `&str` / `&T`
//! are deserialized owned and borrowed). The return value becomes the tool
//! result: `Result<T, E>` → `T` or an error result with `E: Display`;
//! `ToolResult` → passed through; `()` → "ok"; anything else → serde JSON
//! (strings verbatim).
//!
//! Type mapping for the input schema:
//! ```text
//! String / &str                  → {"type":"string"}
//! i8..i128/u8..u128/usize/isize  → {"type":"integer"}
//! f32/f64                        → {"type":"number"}
//! bool                           → {"type":"boolean"}
//! Vec<String>                    → {"type":"array","items":{"type":"string"}}
//! Vec<_>                         → {"type":"array"}
//! Option<T>                      → same as T but NOT added to `required`
//! anything else                  → {"type":"object"}
//! ```
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, Attribute, FnArg, GenericArgument, ItemFn, Lit, Meta, Pat, PathArguments,
    ReturnType, Type,
};

#[proc_macro_attribute]
pub fn claude_tool(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    TokenStream::from(impl_claude_tool(input))
}

struct Param {
    name: String,
    ident: syn::Ident,
    /// Type to deserialize into.
    owned: TokenStream2,
    /// Pass `&value` instead of `value`.
    borrowed: bool,
    optional: bool,
    schema: TokenStream2,
}

fn impl_claude_tool(func: ItemFn) -> TokenStream2 {
    let fn_name = &func.sig.ident;
    let def_fn_name = format_ident!("{}_tool_def", fn_name);
    let tool_fn_name = format_ident!("{}_tool", fn_name);
    let name_str = fn_name.to_string();
    let description = extract_doc(&func.attrs);

    if !func.sig.generics.params.is_empty() {
        return compile_error(func.sig.generics.span(), "#[claude_tool] does not support generic functions", &func);
    }

    let mut params = vec![];
    for input in &func.sig.inputs {
        let FnArg::Typed(pat_type) = input else {
            return compile_error(input.span(), "#[claude_tool] does not support methods", &func);
        };
        let Pat::Ident(p) = pat_type.pat.as_ref() else {
            return compile_error(pat_type.pat.span(), "#[claude_tool] parameters must be plain identifiers", &func);
        };
        let (optional, schema) = type_to_schema(&pat_type.ty);
        let (owned, borrowed) = owned_type(&pat_type.ty);
        params.push(Param {
            name: p.ident.to_string(),
            ident: format_ident!("__arg_{}", p.ident),
            owned,
            borrowed,
            optional,
            schema,
        });
    }

    let prop_entries = params.iter().map(|p| {
        let (n, s) = (&p.name, &p.schema);
        quote! { #n: #s }
    });
    let required = params.iter().filter(|p| !p.optional).map(|p| &p.name);
    let schema_ts = quote! {
        ::claude_sdk::__private::serde_json::json!({
            "type": "object",
            "properties": { #(#prop_entries),* },
            "required": [#(#required),*]
        })
    };

    let decode = params.iter().map(|p| {
        let (n, id, ty) = (&p.name, &p.ident, &p.owned);
        quote! {
            let #id: #ty = match ::claude_sdk::__private::serde_json::from_value(
                __args.get(#n).cloned().unwrap_or(::claude_sdk::__private::serde_json::Value::Null),
            ) {
                ::core::result::Result::Ok(v) => v,
                ::core::result::Result::Err(e) => {
                    return ::claude_sdk::ToolResult::error(::std::format!("invalid argument `{}`: {}", #n, e));
                }
            };
        }
    });
    let call_args = params.iter().map(|p| {
        let id = &p.ident;
        if p.borrowed { quote! { &#id } } else { quote! { #id } }
    });
    let call = if func.sig.asyncness.is_some() {
        quote! { #fn_name(#(#call_args),*).await }
    } else {
        quote! { #fn_name(#(#call_args),*) }
    };

    let serialize = quote! {
        |v| match ::claude_sdk::__private::serde_json::to_value(v) {
            ::core::result::Result::Ok(v) => ::claude_sdk::ToolResult::from_value(v),
            ::core::result::Result::Err(e) => ::claude_sdk::ToolResult::error(e.to_string()),
        }
    };
    let convert = match return_kind(&func.sig.output) {
        ReturnKind::Unit => quote! { { #call; ::claude_sdk::ToolResult::text("ok") } },
        ReturnKind::ToolResult => quote! { #call },
        ReturnKind::Result => quote! {
            match #call {
                ::core::result::Result::Ok(v) => (#serialize)(v),
                ::core::result::Result::Err(e) => ::claude_sdk::ToolResult::error(e.to_string()),
            }
        },
        ReturnKind::Value => quote! { (#serialize)(#call) },
    };

    quote! {
        #func

        /// Auto-generated by `#[claude_tool]` — the ToolDef for this function.
        pub fn #def_fn_name() -> ::claude_sdk::protocol::ToolDef {
            ::claude_sdk::protocol::ToolDef {
                name: #name_str.into(),
                description: #description.into(),
                input_schema: ::core::option::Option::Some(#schema_ts),
                ..::core::default::Default::default()
            }
        }

        /// Auto-generated by `#[claude_tool]` — ToolDef plus a handler that
        /// decodes the arguments and calls this function.
        pub fn #tool_fn_name() -> ::claude_sdk::SdkTool {
            ::claude_sdk::SdkTool::new(#def_fn_name(), |__args: ::claude_sdk::__private::serde_json::Value| async move {
                #(#decode)*
                #convert
            })
        }
    }
}

fn compile_error(span: proc_macro2::Span, msg: &str, func: &ItemFn) -> TokenStream2 {
    quote_spanned! { span => ::core::compile_error!(#msg); #func }
}

enum ReturnKind {
    Unit,
    ToolResult,
    Result,
    Value,
}

fn return_kind(output: &ReturnType) -> ReturnKind {
    let ReturnType::Type(_, ty) = output else {
        return ReturnKind::Unit;
    };
    match ty.as_ref() {
        Type::Tuple(t) if t.elems.is_empty() => ReturnKind::Unit,
        Type::Path(tp) => match tp.path.segments.last().map(|s| s.ident.to_string()).as_deref() {
            Some("ToolResult") => ReturnKind::ToolResult,
            Some("Result") => ReturnKind::Result,
            _ => ReturnKind::Value,
        },
        _ => ReturnKind::Value,
    }
}

/// Owned type to deserialize a parameter into, and whether to pass it by reference.
fn owned_type(ty: &Type) -> (TokenStream2, bool) {
    match ty {
        Type::Reference(r) => match r.elem.as_ref() {
            Type::Path(tp) if tp.path.is_ident("str") => (quote! { ::std::string::String }, true),
            elem => (quote! { #elem }, true),
        },
        other => (quote! { #other }, false),
    }
}

/// Extract the concatenated doc comment from `///` attributes.
fn extract_doc(attrs: &[Attribute]) -> String {
    let mut lines = vec![];
    for attr in attrs {
        if attr.path().is_ident("doc")
            && let Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(el) = &nv.value
            && let Lit::Str(s) = &el.lit
        {
            lines.push(s.value().trim().to_string());
        }
    }
    lines.join(" ")
}

/// Returns (is_optional, json_schema_tokens).
fn type_to_schema(ty: &Type) -> (bool, TokenStream2) {
    match ty {
        Type::Path(tp) => {
            let Some(last) = tp.path.segments.last() else {
                return (false, quote! { {"type": "object"} });
            };
            let name = last.ident.to_string();

            // Option<T> → unwrap inner, mark optional
            if name == "Option" {
                if let PathArguments::AngleBracketed(ab) = &last.arguments
                    && let Some(GenericArgument::Type(inner)) = ab.args.first()
                {
                    let (_, inner_schema) = type_to_schema(inner);
                    return (true, inner_schema);
                }
                return (true, quote! { {"type": "object"} });
            }

            // Vec<T>
            if name == "Vec" {
                if let PathArguments::AngleBracketed(ab) = &last.arguments
                    && let Some(GenericArgument::Type(Type::Path(itp))) = ab.args.first()
                    && let Some(iseg) = itp.path.segments.last()
                    && (iseg.ident == "String" || iseg.ident == "str")
                {
                    return (false, quote! { {"type": "array", "items": {"type": "string"}} });
                }
                return (false, quote! { {"type": "array"} });
            }

            let schema = match name.as_str() {
                "String" | "str" => quote! { {"type": "string"} },
                "bool" => quote! { {"type": "boolean"} },
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
                | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => quote! { {"type": "integer"} },
                "f32" | "f64" => quote! { {"type": "number"} },
                _ => quote! { {"type": "object"} },
            };
            (false, schema)
        }
        Type::Reference(tr) => type_to_schema(&tr.elem),
        _ => (false, quote! { {"type": "object"} }),
    }
}
