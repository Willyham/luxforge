//! Open an https page in the person's default browser through the platform's own opener. The
//! caller runs it off the update loop. Opening a page publishes nothing: a prefilled report is
//! reviewed and submitted, or not, by the person on the page itself.
use std::process::{Command, Stdio};

/// Only an https page is opened, as one argument, so a page can never name a local file or a
/// second argument to the opener.
pub(crate) fn checked(url: &str) -> Result<&str, String> {
    if url.starts_with("https://") && url.len() <= 8192 && url.bytes().all(|b| b.is_ascii_graphic())
    {
        Ok(url)
    } else {
        Err("Only an https page can be opened".into())
    }
}

#[cfg(target_os = "macos")]
fn opener(url: &str) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.arg(url);
    command
}

#[cfg(target_os = "windows")]
fn opener(url: &str) -> Command {
    let mut command = Command::new("rundll32");
    command.args(["url.dll,FileProtocolHandler", url]);
    command
}

#[cfg(all(unix, not(target_os = "macos")))]
fn opener(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}

/// Hand `url` to the platform opener and wait for it to return, which it does once the browser
/// has the page. Blocks: call it inside a task, never on the update loop.
pub(crate) fn open(url: &str) -> Result<(), String> {
    let status = opener(checked(url)?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("Could not open the browser: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Could not open the browser: the opener exited with {status}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_https_token_is_handed_to_the_opener() {
        assert!(checked("https://github.com/Willyham/luxforge/issues/new?title=a%20b").is_ok());
        for refused in [
            "http://example.com",
            "file:///etc/passwd",
            "/Applications/Calculator.app",
            "https://example.com/a b",
            "https://example.com/\n--args",
            "",
        ] {
            assert!(checked(refused).is_err(), "{refused}");
        }
        assert!(checked(&format!("https://{}", "a".repeat(8200))).is_err());
    }
}
