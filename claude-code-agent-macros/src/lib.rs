//! `#[claude_tool]` — turn a (async) fn into a Claude tool at compile time.
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
//! pub fn search_files_tool_def() -> claude_code_agent::protocol::ToolDef { ... }
//! pub fn search_files_tool() -> claude_code_agent::SdkTool { ... } // def + handler
//! ```
//! Register the tool with `SdkMcpServer::new("name").tool(search_files_tool())`.
//!
//! Arguments are deserialized with serde (`T: DeserializeOwned`). A top-level
//! reference is deserialized owned and borrowed: `&str` from `String`, `&[T]`
//! from `Vec<T>`, `&Path` from `PathBuf` (`<Path as ToOwned>::Owned`), `&T`
//! from `T`. References nested in another type (`Option<&str>`, `Vec<&str>`),
//! `&mut T` and `impl Trait` parameters are rejected; use the owned type.
//!
//! The return value becomes the tool result, chosen by its type (aliases
//! included): `Result<T, E>` → `T` or an error result with `E: Display`;
//! `ToolResult` → passed through; `()` → "ok"; anything else → serde JSON
//! (strings verbatim).
//!
//! Type mapping for the input schema (by the last path segment's name):
//! ```text
//! String / str / char / PathBuf / Path       → {"type":"string"}
//! i8..i128/u8..u128/usize/isize              → {"type":"integer"}
//! f32/f64                                    → {"type":"number"}
//! bool                                       → {"type":"boolean"}
//! Vec/VecDeque/HashSet/BTreeSet<T>, [T; N], [T]
//!                                            → {"type":"array","items":<T>}
//!                                              ("items" omitted when T maps to {})
//! HashMap / BTreeMap                         → {"type":"object"}
//! Option<T> (parameter)                      → same as T but NOT added to `required`
//! Option<T> (nested)                         → {"anyOf":[<T>,{"type":"null"}]}
//! anything else (serde_json::Value, OsString, user structs and enums)
//!                                            → {} (any value)
//! ```
//! Give a user type a precise schema by building the `ToolDef` by hand.
#![warn(missing_docs)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::{
    parse_macro_input, Attribute, FnArg, GenericArgument, ItemFn, Lit, Meta, Pat, PathArguments, Type,
};

/// Generate `<fn>_tool_def()` and `<fn>_tool()` for a fn; see the crate docs.
/// Takes no arguments; the fn's doc comment becomes the tool description.
#[proc_macro_attribute]
pub fn claude_tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let attr = TokenStream2::from(attr);
    let expanded = if attr.is_empty() {
        impl_claude_tool(&func)
    } else {
        Err(syn::Error::new_spanned(attr, "#[claude_tool] takes no arguments"))
    };
    // On error keep the fn itself so its callers do not cascade into more errors.
    expanded.unwrap_or_else(|e| {
        let e = e.to_compile_error();
        quote! { #e #func }
    })
    .into()
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

fn impl_claude_tool(func: &ItemFn) -> syn::Result<TokenStream2> {
    let fn_name = &func.sig.ident;
    let def_fn_name = format_ident!("{}_tool_def", fn_name);
    let tool_fn_name = format_ident!("{}_tool", fn_name);
    let name_str = fn_name.unraw().to_string();
    let description = extract_doc(&func.attrs);

    if !func.sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(&func.sig.generics, "#[claude_tool] does not support generic functions"));
    }

    let mut params = vec![];
    for input in &func.sig.inputs {
        let FnArg::Typed(pat_type) = input else {
            return Err(syn::Error::new_spanned(input, "#[claude_tool] does not support methods"));
        };
        let Pat::Ident(p) = pat_type.pat.as_ref() else {
            return Err(syn::Error::new_spanned(&pat_type.pat, "#[claude_tool] parameters must be plain identifiers"));
        };
        let (owned, borrowed) = owned_type(&pat_type.ty)?;
        let (optional, schema) = type_to_schema(&pat_type.ty);
        params.push(Param {
            name: p.ident.unraw().to_string(),
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
        ::claude_code_agent::__private::serde_json::json!({
            "type": "object",
            "properties": { #(#prop_entries),* },
            "required": [#(#required),*]
        })
    };

    let decode = params.iter().map(|p| {
        let (n, id, ty) = (&p.name, &p.ident, &p.owned);
        quote! {
            let #id: #ty = match ::claude_code_agent::__private::serde_json::from_value(
                __args.get(#n).cloned().unwrap_or(::claude_code_agent::__private::serde_json::Value::Null),
            ) {
                ::core::result::Result::Ok(v) => v,
                ::core::result::Result::Err(e) => {
                    return ::claude_code_agent::ToolResult::error(::std::format!("invalid argument `{}`: {}", #n, e));
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

    // The impl picked depends on the returned value's type; see
    // `claude_code_agent::__private::ret`.
    let convert = quote! {{
        #[allow(unused_imports)]
        use ::claude_code_agent::__private::ret::{
            ResultKind as _, SerializeKind as _, ToolResultKind as _, UnitKind as _,
        };
        let __ret = #call;
        (&__ret).__claude_tool_kind().convert(__ret)
    }};

    Ok(quote! {
        #func

        /// Auto-generated by `#[claude_tool]` — the ToolDef for this function.
        pub fn #def_fn_name() -> ::claude_code_agent::protocol::ToolDef {
            ::claude_code_agent::protocol::ToolDef {
                name: #name_str.into(),
                description: #description.into(),
                input_schema: ::core::option::Option::Some(#schema_ts),
                ..::core::default::Default::default()
            }
        }

        /// Auto-generated by `#[claude_tool]` — ToolDef plus a handler that
        /// decodes the arguments and calls this function.
        pub fn #tool_fn_name() -> ::claude_code_agent::SdkTool {
            ::claude_code_agent::SdkTool::new(#def_fn_name(), |__args: ::claude_code_agent::__private::serde_json::Value| async move {
                #(#decode)*
                #convert
            })
        }
    })
}

/// Owned type to deserialize a parameter into, and whether to pass it by reference.
fn owned_type(ty: &Type) -> syn::Result<(TokenStream2, bool)> {
    let Type::Reference(r) = ty else {
        reject_unsupported(ty)?;
        return Ok((quote! { #ty }, false));
    };
    if r.mutability.is_some() {
        return Err(syn::Error::new_spanned(
            ty,
            "#[claude_tool] does not support `&mut` parameters; take the owned type (`T` instead of `&mut T`)",
        ));
    }
    let elem = r.elem.as_ref();
    reject_unsupported(elem)?;
    let owned = match elem {
        Type::Path(tp) if tp.path.is_ident("str") => quote! { ::std::string::String },
        Type::Slice(s) => {
            let inner = &s.elem;
            quote! { ::std::vec::Vec<#inner> }
        }
        Type::Path(tp) if tp.path.segments.last().is_some_and(|s| s.ident == "Path") => {
            quote! { <#elem as ::std::borrow::ToOwned>::Owned }
        }
        _ => quote! { #elem },
    };
    Ok((owned, true))
}

/// Reject types that cannot be deserialized owned: references (other than a
/// top-level one, handled by `owned_type`), `impl Trait` and `dyn Trait`.
fn reject_unsupported(ty: &Type) -> syn::Result<()> {
    match ty {
        Type::Reference(_) => Err(syn::Error::new_spanned(
            ty,
            "#[claude_tool] parameters cannot hold references inside another type; use the owned type \
             (`Option<String>` instead of `Option<&str>`, `Vec<String>` instead of `Vec<&str>`)",
        )),
        Type::ImplTrait(_) => Err(syn::Error::new_spanned(
            ty,
            "#[claude_tool] does not support `impl Trait` parameters; use a concrete owned type \
             (`String` instead of `impl Into<String>`)",
        )),
        Type::TraitObject(_) => Err(syn::Error::new_spanned(
            ty,
            "#[claude_tool] parameters cannot be trait objects; use a concrete owned type",
        )),
        Type::Path(tp) => {
            for seg in &tp.path.segments {
                if let PathArguments::AngleBracketed(ab) = &seg.arguments {
                    for arg in &ab.args {
                        if let GenericArgument::Type(t) = arg {
                            reject_unsupported(t)?;
                        }
                    }
                }
            }
            Ok(())
        }
        Type::Tuple(t) => t.elems.iter().try_for_each(reject_unsupported),
        Type::Array(a) => reject_unsupported(&a.elem),
        Type::Slice(s) => reject_unsupported(&s.elem),
        Type::Paren(p) => reject_unsupported(&p.elem),
        Type::Group(g) => reject_unsupported(&g.elem),
        _ => Ok(()),
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

/// Returns `(is_optional, json_schema_tokens)` for a parameter type.
fn type_to_schema(ty: &Type) -> (bool, TokenStream2) {
    let (optional, schema) = match option_inner(ty) {
        Some(inner) => (true, inner.and_then(schema)),
        None => (false, schema(ty)),
    };
    (optional, schema.unwrap_or_else(|| quote! { {} }))
}

/// `Some(first type argument)` if `ty` is `Option<_>`.
fn option_inner(ty: &Type) -> Option<Option<&Type>> {
    let Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    (last.ident == "Option").then(|| first_type_arg(last))
}

fn first_type_arg(seg: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(ab) = &seg.arguments else { return None };
    ab.args.iter().find_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// JSON schema for a type; `None` means no constraint (`{}`), which is never
/// wrong, unlike a guessed type the model would then follow.
fn schema(ty: &Type) -> Option<TokenStream2> {
    match ty {
        Type::Reference(r) => schema(&r.elem),
        Type::Paren(p) => schema(&p.elem),
        Type::Group(g) => schema(&g.elem),
        Type::Slice(s) => Some(array_schema(schema(&s.elem))),
        Type::Array(a) => Some(array_schema(schema(&a.elem))),
        Type::Path(tp) => {
            let last = tp.path.segments.last()?;
            match last.ident.to_string().as_str() {
                "String" | "str" | "char" | "PathBuf" | "Path" => Some(quote! { {"type": "string"} }),
                "bool" => Some(quote! { {"type": "boolean"} }),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
                | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => Some(quote! { {"type": "integer"} }),
                "f32" | "f64" => Some(quote! { {"type": "number"} }),
                "Vec" | "VecDeque" | "HashSet" | "BTreeSet" => {
                    Some(array_schema(first_type_arg(last).and_then(schema)))
                }
                "HashMap" | "BTreeMap" => Some(quote! { {"type": "object"} }),
                "Option" => {
                    let inner = first_type_arg(last).and_then(schema)?;
                    Some(quote! { {"anyOf": [#inner, {"type": "null"}]} })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn array_schema(items: Option<TokenStream2>) -> TokenStream2 {
    match items {
        Some(items) => quote! { {"type": "array", "items": #items} },
        None => quote! { {"type": "array"} },
    }
}
