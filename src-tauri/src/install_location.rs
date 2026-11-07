//! macOS: where the app runs from, and moving it to /Applications.
//!
//! Run from a mounted .dmg or from Downloads, the app is hard to find again (no Dock
//! icon, and Launchpad/Spotlight don't list it) and in-app updates can't replace it:
//! the .dmg is read-only, and macOS runs a quarantined app opened from Downloads from
//! a hidden read-only copy ("App Translocation"). On launch we offer to move it.
//! Dialogs go through `osascript` so no dialog plugin is needed.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum Location {
    /// /Applications (or a subfolder) or ~/Applications.
    Applications,
    /// A hidden read-only copy macOS made of a quarantined app.
    Translocated,
    /// A mounted disk image (read-only).
    DiskImage,
    /// Anywhere else (Downloads, Desktop, …): works, but easy to lose.
    Elsewhere,
}

/// `…/MagicLyrics.app` from the running executable `…/MagicLyrics.app/Contents/MacOS/x`.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
}

pub fn classify(bundle: &Path, home: Option<&Path>) -> Location {
    let s = bundle.to_string_lossy();
    if s.contains("/AppTranslocation/") {
        Location::Translocated
    } else if bundle.starts_with("/Volumes") {
        Location::DiskImage
    } else if bundle.starts_with("/Applications") || home.is_some_and(|h| bundle.starts_with(h.join("Applications"))) {
        Location::Applications
    } else {
        Location::Elsewhere
    }
}

/// "0.2.5" < "0.2.10".
fn version_parts(v: &str) -> Vec<u64> {
    v.trim().split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(a: &str, b: &str) -> bool {
    version_parts(a) > version_parts(b)
}

/// CFBundleShortVersionString from an app bundle's (XML) Info.plist.
fn bundle_version(bundle: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).ok()?;
    let re = regex::Regex::new(r"<key>CFBundleShortVersionString</key>\s*<string>([^<]+)</string>").ok()?;
    re.captures(&plist).map(|c| c[1].trim().to_string())
}

/// `'…'` for /bin/sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `"…"` for AppleScript.
fn as_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// Copy `src` to `dest` (replacing what's there only once the copy is complete) and
/// clear the quarantine flag, so macOS doesn't run the copy translocated again.
fn copy_script(src: &Path, dest: &Path) -> String {
    let (src, dest) = (src.to_string_lossy(), dest.to_string_lossy());
    let tmp = format!("{}.part", dest);
    let old = format!("{}.old", dest);
    let (src, dest, tmp, old) = (sh_quote(&src), sh_quote(&dest), sh_quote(&tmp), sh_quote(&old));
    format!(
        "set -e; rm -rf {tmp} {old}; /usr/bin/ditto {src} {tmp}; \
         /usr/bin/xattr -dr com.apple.quarantine {tmp} || true; \
         if [ -e {dest} ]; then mv {dest} {old}; fi; mv {tmp} {dest}; rm -rf {old}"
    )
}

/// Why an in-app update can't work from here, if it can't.
pub fn update_blocker() -> Option<String> {
    let bundle = bundle_of(&std::env::current_exe().ok()?)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match classify(&bundle, home.as_deref()) {
        Location::DiskImage => Some(
            "MagicLyrics is running from the disk image, which can't be changed. \
             Drag it to Applications, open it from there, then update."
                .into(),
        ),
        Location::Translocated => Some(
            "macOS is running MagicLyrics from a temporary read-only copy. \
             Move it to Applications, open it from there, then update."
                .into(),
        ),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub use mac::offer_move;

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use crate::AppState;
    use std::process::Command;
    use std::sync::atomic::Ordering;
    use tauri::{AppHandle, Manager};

    /// Show a dialog and return the label of the clicked button (None: closed or failed).
    fn dialog(text: &str, buttons: &[&str]) -> Option<String> {
        let list = buttons.iter().map(|b| as_quote(b)).collect::<Vec<_>>().join(", ");
        let script = format!(
            "display dialog {} with title \"MagicLyrics\" buttons {{{list}}} default button {} with icon note",
            as_quote(text),
            buttons.len()
        );
        let out = Command::new("/usr/bin/osascript").arg("-e").arg(script).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().strip_prefix("button returned:").map(str::to_string)
    }

    fn alert(text: &str) {
        let _ = dialog(text, &["OK"]);
    }

    /// Run the copy as the user, or with an admin password prompt if that's not allowed.
    fn copy_app(src: &Path, dest: &Path) -> bool {
        let script = copy_script(src, dest);
        if Command::new("/bin/sh").arg("-c").arg(&script).status().is_ok_and(|s| s.success()) {
            return true;
        }
        let elevated = format!("do shell script {} with administrator privileges", as_quote(&script));
        Command::new("/usr/bin/osascript").arg("-e").arg(elevated).status().is_ok_and(|s| s.success())
    }

    /// Start `bundle` as a new instance once this one has quit, then quit.
    fn relaunch(app: &AppHandle, bundle: &Path) {
        let _ = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 1; /usr/bin/open -n \"$0\"")
            .arg(bundle)
            .spawn();
        crate::save_and_exit(app);
    }

    fn dont_ask_again(app: &AppHandle) {
        let st = app.state::<AppState>();
        st.settings.lock().unwrap().ask_move_to_applications = false;
        st.settings_dirty.store(true, Ordering::Relaxed);
    }

    /// On launch outside an Applications folder, offer to move there. Blocks on the
    /// dialog, so call it from a background thread.
    pub fn offer_move(app: AppHandle) {
        if cfg!(debug_assertions) || !app.state::<AppState>().settings.lock().unwrap().ask_move_to_applications {
            return;
        }
        let Some(bundle) = std::env::current_exe().ok().as_deref().and_then(bundle_of) else { return };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let loc = classify(&bundle, home.as_deref());
        if loc == Location::Applications {
            return;
        }
        let Some(name) = bundle.file_name() else { return };
        let dest = Path::new("/Applications").join(name);
        let ours = app.package_info().version.to_string();
        // Some(None): a copy is there but its version is unknown; never overwrite that.
        let found = dest.exists().then(|| bundle_version(&dest));
        let installed = found.clone().flatten();

        // Already installed and at least as new: use that copy instead of this one.
        let keep_installed = match &found {
            Some(Some(v)) => !is_newer(&ours, v),
            Some(None) => true,
            None => false,
        };
        if keep_installed {
            let which = installed.map(|v| format!("MagicLyrics v{v}")).unwrap_or_else(|| "MagicLyrics".into());
            let text = format!("{which} is already in your Applications folder. Open that one instead of this copy?");
            if dialog(&text, &["Keep This Copy", "Open from Applications"]).as_deref() == Some("Open from Applications") {
                relaunch(&app, &dest);
            }
            return;
        }

        let text = match &installed {
            Some(v) => format!(
                "Replace MagicLyrics v{v} in your Applications folder with this version (v{ours})?"
            ),
            None => "Put MagicLyrics in your Applications folder?\n\n\
                     That way you can always find it in Launchpad and Spotlight (opening it again \
                     brings the lyrics back on screen), and in-app updates can install."
                .into(),
        };
        let go = if installed.is_some() { "Replace" } else { "Install in Applications" };
        match dialog(&text, &["Don't Ask Again", "Not Now", go]).as_deref() {
            Some(b) if b == go => {
                // The original is left where it is; opening it later offers the installed copy.
                if !copy_app(&bundle, &dest) {
                    alert("Couldn't copy MagicLyrics to Applications. Drag it there from the disk image or Finder instead.");
                    return;
                }
                relaunch(&app, &dest);
            }
            Some("Don't Ask Again") => dont_ask_again(&app),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_locations() {
        let home = Path::new("/Users/me");
        let c = |p: &str| classify(Path::new(p), Some(home));
        assert_eq!(c("/Applications/MagicLyrics.app"), Location::Applications);
        assert_eq!(c("/Applications/Utilities/MagicLyrics.app"), Location::Applications);
        assert_eq!(c("/Users/me/Applications/MagicLyrics.app"), Location::Applications);
        assert_eq!(c("/Volumes/MagicLyrics/MagicLyrics.app"), Location::DiskImage);
        assert_eq!(
            c("/private/var/folders/x/T/AppTranslocation/1A2B/d/MagicLyrics.app"),
            Location::Translocated
        );
        assert_eq!(c("/Users/me/Downloads/MagicLyrics.app"), Location::Elsewhere);
        assert_eq!(c("/ApplicationsOld/MagicLyrics.app"), Location::Elsewhere);
    }

    #[test]
    fn finds_bundle_and_compares_versions() {
        let exe = Path::new("/Applications/MagicLyrics.app/Contents/MacOS/magiclyrics");
        assert_eq!(bundle_of(exe).unwrap(), Path::new("/Applications/MagicLyrics.app"));
        assert!(bundle_of(Path::new("/usr/bin/x")).is_none());
        assert!(is_newer("0.2.10", "0.2.5"));
        assert!(!is_newer("0.2.5", "0.2.5"));
        assert!(!is_newer("0.2.4", "0.2.5"));
    }

    #[test]
    fn quotes_for_shell_and_applescript() {
        assert_eq!(sh_quote("a'b"), r"'a'\''b'");
        assert_eq!(as_quote(r#"say "hi" \ ok"#), r#""say \"hi\" \\ ok""#);
        let s = copy_script(Path::new("/Volumes/M L/MagicLyrics.app"), Path::new("/Applications/MagicLyrics.app"));
        assert!(s.contains("/usr/bin/ditto '/Volumes/M L/MagicLyrics.app' '/Applications/MagicLyrics.app.part'"));
    }
}
