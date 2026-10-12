//! The attributes the workspace's tests are marked with. Each leaves the test exactly as written;
//! `cargo xtask check` reads the mark from the source.

use proc_macro::{TokenStream, TokenTree};

/// Marks a test that takes 100 ms or more, or that waits on a host service such as FSEvents:
/// `cargo xtask check --quick` leaves it out, and the whole `check` runs it. Write it after
/// `#[test]`, on its own line, among the test's attributes:
///
/// ```text
/// #[test]
/// #[slow]
/// fn every_code_round_trips() {}
/// ```
#[proc_macro_attribute]
pub fn slow(arguments: TokenStream, item: TokenStream) -> TokenStream {
    let function = item
        .clone()
        .into_iter()
        .any(|token| matches!(token, TokenTree::Ident(ref ident) if ident.to_string() == "fn"));
    let refusal = if !arguments.is_empty() {
        Some("`#[slow]` takes no arguments")
    } else if !function {
        Some("`#[slow]` marks a test function")
    } else {
        None
    };
    match refusal {
        Some(message) => {
            let mut refused: TokenStream = format!("compile_error!({message:?});")
                .parse()
                .expect("a compile_error! invocation");
            refused.extend(item);
            refused
        }
        None => item,
    }
}
