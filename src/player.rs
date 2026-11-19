//! An embedded YouTube player for Linux.
//!
//! The list and swipe pages hand a track to this module. The player is a
//! normal page in the main window: it looks up the first YouTube result and
//! loads the embedded video without opening a browser or a second window.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use webkit6::prelude::*;

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/131.0 Safari/537.36";

/// The player page shown inside the main window.
#[derive(Clone)]
pub struct PlayerView {
    root: gtk::Box,
    bar: gtk::Box,
    title: gtk::Label,
    webview: webkit6::WebView,
    back: gtk::Button,
    external_url: Rc<std::cell::RefCell<String>>,
    /// Incremented for every new request, so a slow lookup for an old card
    /// can never replace the video of the card now on screen.
    play_token: Rc<Cell<u64>>,
}

impl PlayerView {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        bar.set_margin_top(10);
        bar.set_margin_bottom(10);
        bar.set_margin_start(14);
        bar.set_margin_end(14);

        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        back.add_css_class("flat");
        back.set_tooltip_text(Some("Back to your picks"));

        let title = gtk::Label::new(Some("YouTube player"));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("heading");

        let external = gtk::Button::with_label("Open in browser");
        external.add_css_class("flat");
        external.set_tooltip_text(Some("Open this search in your normal browser"));

        bar.append(&back);
        bar.append(&title);
        bar.append(&external);

        let settings = webkit6::Settings::new();
        settings.set_enable_media(true);
        settings.set_enable_mediasource(true);
        settings.set_media_playback_requires_user_gesture(false);

        let policies = webkit6::WebsitePolicies::builder()
            .autoplay(webkit6::AutoplayPolicy::Allow)
            .build();
        let webview = webkit6::WebView::builder()
            .settings(&settings)
            .website_policies(&policies)
            .build();
        webview.set_vexpand(true);
        webview.set_hexpand(true);

        root.append(&bar);
        root.append(&webview);

        let external_url = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let external_for_click = external_url.clone();
        external.connect_clicked(move |_| {
            let url = external_for_click.borrow().clone();
            if !url.is_empty() {
                let _ = gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>);
            }
        });

        let player = Self {
            root,
            bar,
            title,
            webview,
            back,
            external_url,
            play_token: Rc::new(Cell::new(0)),
        };
        player.show_placeholder();
        player
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Hide the page bar when the player is embedded in a song card.
    pub fn set_bar_visible(&self, visible: bool) {
        self.bar.set_visible(visible);
    }

    /// Stop playback and clear the current video.
    pub fn stop(&self) {
        self.play_token.set(self.play_token.get().wrapping_add(1));
        self.title.set_text("YouTube player");
        self.external_url.borrow_mut().clear();
        self.show_placeholder();
    }

    pub fn back_button(&self) -> &gtk::Button {
        &self.back
    }

    pub fn connect_back<F: Fn() + 'static>(&self, callback: F) {
        self.back.connect_clicked(move |_| callback());
    }

    /// Load (or reload) the player for `query`, with `title` in the page bar.
    pub fn play(&self, title: &str, query: &str) {
        let token = self.play_token.get().wrapping_add(1);
        self.play_token.set(token);

        self.title.set_text(title);
        let search = search_url(query);
        *self.external_url.borrow_mut() = search.clone();

        // Show a quiet placeholder immediately so the page never flashes a
        // white web view while the video id is being resolved.
        self.webview.load_html(
            r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1">
<style>html,body{margin:0;height:100%;background:#0e0e10;color:#b8b8bd;font:15px system-ui,sans-serif}
body{display:flex;align-items:center;justify-content:center}</style></head><body>Looking up the song…</body></html>"#,
            Some("http://localhost/"),
        );

        let (tx, rx) = async_channel::bounded(1);
        let query = query.to_string();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(resolve_video_id(&query));
        });

        let webview = self.webview.clone();
        let title = self.title.clone();
        let fallback = search.clone();
        let play_token = self.play_token.clone();
        glib::MainContext::default().spawn_local(async move {
            match rx.recv().await {
                _ if play_token.get() != token => {}
                Ok(Some(video_id)) => {
                    webview.load_html(&player_html(&video_id), Some("http://localhost/"));
                }
                _ => {
                    title.set_text("YouTube search");
                    webview.load_uri(&fallback);
                }
            }
        });
    }

    fn show_placeholder(&self) {
        self.webview.load_html(
            r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1">
<style>html,body{margin:0;height:100%;background:#0e0e10;color:#b8b8bd;font:15px system-ui,sans-serif}
body{display:flex;align-items:center;justify-content:center;text-align:center;padding:24px;box-sizing:border-box}</style>
</head><body>Pick a song and press its YouTube button to play it here.</body></html>"#,
            Some("http://localhost/"),
        );
    }
}

fn search_url(query: &str) -> String {
    format!(
        "https://www.youtube.com/results?search_query={}",
        urlencoding::encode(query)
    )
}

/// YouTube rejects a bare `/embed` URL (error 153) when the document has no
/// real origin, so wrap the iframe in a tiny local page and give it one.
fn player_html(video_id: &str) -> String {
    format!(
        r#"<!doctype html>
<html><head><meta name="viewport" content="width=device-width,initial-scale=1">
<style>html,body{{margin:0;background:#000}}iframe{{position:fixed;inset:0;width:100%;height:100%;border:0}}</style>
</head><body><iframe id="player" title="YouTube player"
src="https://www.youtube.com/embed/{video_id}?autoplay=1&rel=0&playsinline=1&enablejsapi=1"
allow="autoplay; encrypted-media; picture-in-picture" allowfullscreen></iframe>
<script>
const frame = document.getElementById('player');
function nudge() {{
  frame.contentWindow.postMessage('{{"event":"command","func":"playVideo","args":""}}', '*');
}}
setTimeout(nudge, 1200);
setTimeout(nudge, 3000);
</script>
</body></html>"#
    )
}

fn resolve_video_id(query: &str) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(fetch_video_id(query))
}

async fn fetch_video_id(query: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let html = client
        .get("https://www.youtube.com/results")
        .query(&[("search_query", query)])
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    first_video_id(&html)
}

/// YouTube's results page embeds the first rendered video a few times in its
/// bootstrap JSON. Extract the first well-formed id.
fn first_video_id(html: &str) -> Option<String> {
    const MARKER: &str = "\"videoId\":\"";
    for (index, _) in html.match_indices(MARKER) {
        let rest = &html[index + MARKER.len()..];
        let Some(end) = rest.find('"') else {
            continue;
        };
        let id = &rest[..end];
        if id.len() == 11
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Some(id.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_first_video_id() {
        let html = r#"{"videoId":"aCyGvGEtOwc","thumbnail":{"videoId":"qV7IhHFnhJc"}}"#;
        assert_eq!(first_video_id(html).as_deref(), Some("aCyGvGEtOwc"));
    }

    #[test]
    fn skips_malformed_ids() {
        let html = r#""videoId":"short" "videoId":"aCyGvGEtOwc""#;
        assert_eq!(first_video_id(html).as_deref(), Some("aCyGvGEtOwc"));
        assert!(first_video_id("no ids here").is_none());
    }
}
