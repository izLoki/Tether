//! Desktop notifications for sessions that need the user while tether is in the background.
//!
//! Their text can come from a program's output (OSC 9 / 777 / 99), so it is cleaned:
//! no control or bidirectional-override characters (which could disguise the text), and
//! on Linux no notification markup. The Windows toast library XML-escapes by itself.

/// Shows a native toast without blocking the UI (WinRT / D-Bus can take a while).
pub fn desktop(title: String, body: String) {
    let (title, body) = (clean(&title), clean(&body));
    let _ = std::thread::Builder::new().name("tether-toast".into()).spawn(move || {
        #[cfg(target_os = "macos")]
        {
            // Set our bundle identity before the library falls back to Finder.
            // OnceLock serializes initialization across simultaneous session alerts.
            static APP: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            APP.get_or_init(|| {
                let _ = notify_rust::set_application("io.github.izloki.tether");
            });
        }
        let _ = notify_rust::Notification::new().appname("tether").summary(&title).body(&body).show();
    });
}

/// Explicit direction marks and overrides (U+200E/F, U+202A..E, U+2066..9).
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `text` safe to show in a native notification (newlines kept).
pub fn clean(text: &str) -> String {
    let plain = text.chars().filter(|c| !is_bidi_control(*c) && (*c == '\n' || !c.is_control()));
    if cfg!(all(unix, not(target_os = "macos"))) {
        // Notification servers render a subset of HTML in the body.
        plain.fold(String::new(), |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                c => out.push(c),
            }
            out
        })
    } else {
        plain.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_is_cleaned() {
        let evil = "Build \u{202E}exe.txt\u{202C} done\x1b[31m\x07\nnext line";
        let out = clean(evil);
        assert!(!out.chars().any(|c| is_bidi_control(c) || (c.is_control() && c != '\n')), "{out:?}");
        assert!(out.contains("next line") && out.contains('\n'));
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert_eq!(clean("<b>x</b> & y"), "&lt;b&gt;x&lt;/b&gt; &amp; y");
        } else {
            assert_eq!(clean("<b>x</b> & y"), "<b>x</b> & y");
        }
    }
}
