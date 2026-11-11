use proc_macro::TokenStream;
use proc_macro2::{Literal, TokenStream as TokenStream2};
use quote::quote;
use syn::{Item, ItemConst, LitInt, LitStr, parse::Parser, parse_macro_input};

/// Making stable API surface more explicit for future development
/// This is only to be used inside rsmalloc's private functions
#[proc_macro_attribute]
pub fn stable_api_surface(args: TokenStream, input: TokenStream) -> TokenStream {
    let args2: TokenStream2 = args.into();
    let mut since: String = String::new();

    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("since") {
            let lit: LitStr = meta.value()?.parse()?;
            since.push_str(lit.value().as_str());
        }
        Ok(())
    });

    let _ = parser.parse2(args2);

    let doc: String = if since.is_empty() {
        format!("Stable api surface")
    } else {
        format!("Stable Since {}", since)
    };

    let item2: TokenStream2 = input.into();

    let doc_lit = Literal::string(doc.as_str());

    quote! {
        #[doc = #doc_lit]
        #item2
    }
    .into()
}

/// Type-sugar, asserting size of a struct or enum
#[proc_macro_attribute]
pub fn assert_sizes(args: TokenStream, input: TokenStream) -> TokenStream {
    let lit = parse_macro_input!(args as LitInt);
    let expected_size: usize = match lit.base10_parse() {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let item = parse_macro_input!(input as Item);

    let expanded = match &item {
        Item::Struct(s) => {
            let name = &s.ident;
            quote! {
                #item
                const _: () = assert!(size_of::<#name>() == #expected_size);
            }
        }
        Item::Enum(e) => {
            let name = &e.ident;
            quote! {
                #item
                const _: () = assert!(size_of::<#name>() == #expected_size);
            }
        }
        _ => {
            quote! {
                compile_error!("`#[assert_size]` can only be used on structs, or enums");
                #item
            }
        }
    };

    expanded.into()
}

#[proc_macro_attribute]
pub fn validate_size_classes(args: TokenStream, input: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "`validate_size_classes` takes no arguments",
        )
        .to_compile_error()
        .into();
    }

    let item = parse_macro_input!(input as ItemConst);
    let name = &item.ident;

    quote! {
        #item

        const _: () = {
            let classes: &[usize] = &#name;
            assert!(!classes.is_empty(), "size classes must not be empty");
            assert!(classes.len() <= u8::MAX as usize, "size class indices must fit in u8");
            assert!(classes[0] == 16, "first size class must be 16 bytes");
            assert!(classes[classes.len() - 1] == 2 * 1024 * 1024, "last size class must be 2 MiB");

            let mut i = 0;
            let mut has_4096 = false;
            while i < classes.len() {
                assert!(classes[i] % 16 == 0, "size classes must be 16-byte aligned");
                if i > 0 {
                    assert!(classes[i - 1] < classes[i], "size classes must be strictly increasing");
                }
                if classes[i] == 4096 {
                    has_4096 = true;
                }
                i += 1;
            }
            assert!(has_4096, "size classes must include 4096");
        };
    }
    .into()
}
