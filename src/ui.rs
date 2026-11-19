//! The libadwaita desktop interface for lbreco.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;

use crate::config;
use crate::export;
use crate::model::{
    APP_ID, AiConfig, Config, DiscoveryConfig, DiscoveryMode, ListenBrainzConfig, Progress,
    Recommendation, RunResult, VERSION,
};
use crate::pipeline;
use crate::playlist::{self, Playlist};

const EMPTY_TITLE: &str = "Find your next 50 songs";
const EMPTY_DESCRIPTION: &str = "lbreco reads your ListenBrainz history, finds real tracks from artists in your lane, and — if you enable it — lets an AI model choose the final 50. Keep the ones you like in a playlist for Spotify or YouTube Music.";

struct AppState {
    config: Config,
    running: bool,
    result: Option<RunResult>,
    cancel: Option<Arc<AtomicBool>>,
    playlist: Playlist,
    swipe_index: usize,
}

struct Ui {
    window: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    stack: adw::ViewStack,
    hero: gtk::Label,
    summary: gtk::Label,
    generate: gtk::Button,
    cancel: gtk::Button,
    preferences: gtk::Button,
    preferences_header: gtk::Button,
    progress: gtk::ProgressBar,
    progress_box: gtk::Box,
    status: gtk::Label,
    empty: adw::StatusPage,
    results_group: adw::PreferencesGroup,
    /// The rows currently shown in `results_group`. AdwPreferencesGroup also
    /// owns an internal header box, so only rows we added may be removed.
    result_rows: RefCell<Vec<adw::ActionRow>>,
    result_checks: RefCell<Vec<gtk::CheckButton>>,
    result_add_buttons: RefCell<Vec<(String, gtk::Button)>>,
    selection_toolbar: gtk::Box,
    selection_label: gtk::Label,
    select_all: gtk::Button,
    clear_selection: gtk::Button,
    add_selected: gtk::Button,

    swipe_empty: adw::StatusPage,
    swipe_content: gtk::Box,
    swipe_done: adw::StatusPage,
    swipe_position: gtk::Label,
    swipe_source: gtk::Label,
    swipe_artist: gtk::Label,
    swipe_title: gtk::Label,
    swipe_meta: gtk::Label,
    swipe_reason: gtk::Label,
    swipe_tags: gtk::Label,
    swipe_video_slot: gtk::Box,
    #[cfg(target_os = "linux")]
    swipe_player: RefCell<Option<crate::player::PlayerView>>,
    #[cfg(target_os = "linux")]
    swipe_video_mbid: RefCell<Option<String>>,
    swipe_previous: gtk::Button,
    swipe_skip: gtk::Button,
    swipe_add: gtk::Button,
    swipe_youtube: gtk::Button,

    playlist_page: adw::ViewStackPage,
    playlist_group: adw::PreferencesGroup,
    playlist_rows: RefCell<Vec<adw::ActionRow>>,
    playlist_empty: adw::StatusPage,
    playlist_count: gtk::Label,
    playlist_clear: gtk::Button,
    playlist_export_csv: gtk::Button,
    playlist_export_json: gtk::Button,
    playlist_export_m3u: gtk::Button,
    playlist_copy: gtk::Button,
    done_restart: gtk::Button,
    done_playlist: gtk::Button,

    #[cfg(target_os = "linux")]
    player: crate::player::PlayerView,
    #[cfg(target_os = "linux")]
    player_return: RefCell<String>,

    action_generate: gio::SimpleAction,
    action_export_json: gio::SimpleAction,
    action_export_csv: gio::SimpleAction,
    action_copy: gio::SimpleAction,
    preferences_action: gio::SimpleAction,
    about_action: gio::SimpleAction,
    quit_action: gio::SimpleAction,
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| {
        // GTK is only initialised once the application itself starts, so the
        // icon has to be registered from here rather than before `run`.
        if let Some(display) = gtk::gdk::Display::default() {
            let theme = gtk::IconTheme::for_display(&display);
            // GTK treats a resource icon path as the root of the hicolor
            // theme, so the layout is `<path>/64x64/apps/...`.
            theme.add_resource_path("/io/github/lbreco/LBreco/icons");
        }
        gtk::Window::set_default_icon_name(APP_ID);
    });

    app.connect_activate(move |app| {
        let state = Rc::new(RefCell::new(AppState {
            config: config::load(),
            running: false,
            result: None,
            cancel: None,
            playlist: playlist::Playlist::load(),
            swipe_index: 0,
        }));
        let ui = Rc::new(build_ui(app));
        connect_actions(&ui, &state);
        ui.stack.set_visible_child_name("list");
        render_playlist(&ui, &state);
        update_swipe(&ui, &state);
        update_summary(&ui, &state);
        if state
            .borrow()
            .config
            .listenbrainz
            .username
            .trim()
            .is_empty()
        {
            open_preferences(&ui, &state);
        }
    });

    app.run()
}

fn build_ui(app: &adw::Application) -> Ui {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(1080)
        .default_height(820)
        .build();
    window.set_icon_name(Some(APP_ID));

    let toast_overlay = adw::ToastOverlay::new();
    let toolbar_view = adw::ToolbarView::new();
    toast_overlay.set_child(Some(&toolbar_view));
    window.set_content(Some(&toast_overlay));

    // The three pages: list, one-card swipe, and the accepted playlist.
    let stack = adw::ViewStack::new();
    stack.set_vexpand(true);
    let switcher = adw::ViewSwitcher::new();
    switcher.set_policy(adw::ViewSwitcherPolicy::Wide);
    switcher.set_stack(Some(&stack));

    // Header bar, with the embedded app icon on the left.
    let header = gtk::HeaderBar::new();
    let brand = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let app_icon = gtk::Image::from_icon_name(APP_ID);
    app_icon.set_pixel_size(22);
    let brand_label = gtk::Label::new(Some("lbreco"));
    brand_label.add_css_class("heading");
    brand.append(&app_icon);
    brand.append(&brand_label);
    header.pack_start(&brand);
    header.set_title_widget(Some(&switcher));

    let actions = gio::SimpleActionGroup::new();
    window.insert_action_group("win", Some(&actions));

    let menu = gio::Menu::new();
    let main_section = gio::Menu::new();
    main_section.append(Some("Generate picks"), Some("win.generate"));
    main_section.append(Some("Preferences"), Some("win.preferences"));
    main_section.append(Some("_About"), Some("win.about"));
    let export_section = gio::Menu::new();
    export_section.append(Some("Export picks as _JSON"), Some("win.export-json"));
    export_section.append(Some("Export picks as _CSV"), Some("win.export-csv"));
    export_section.append(Some("_Copy picks"), Some("win.copy"));
    let quit_section = gio::Menu::new();
    quit_section.append(Some("_Quit"), Some("win.quit"));
    menu.append_section(None, &main_section);
    menu.append_section(None, &export_section);
    menu.append_section(None, &quit_section);
    let menu_popover = gtk::PopoverMenu::from_model(Some(&menu));
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_tooltip_text(Some("Main menu"));
    menu_button.set_popover(Some(&menu_popover));

    let preferences = gtk::Button::with_label("Preferences");
    let preferences_header = gtk::Button::from_icon_name("emblem-system-symbolic");
    preferences_header.set_tooltip_text(Some("Preferences"));
    preferences_header.add_css_class("flat");
    header.pack_end(&menu_button);
    header.pack_end(&preferences_header);
    toolbar_view.add_top_bar(&header);

    // ---- List page -----------------------------------------------------
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(20);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let hero = gtk::Label::new(Some(EMPTY_TITLE));
    hero.set_xalign(0.0);
    hero.set_wrap(true);
    hero.add_css_class("title-1");

    let summary = gtk::Label::new(None);
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.set_selectable(false);
    summary.add_css_class("dim-label");
    summary.add_css_class("body");

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let generate = gtk::Button::with_label("Generate picks");
    generate.add_css_class("suggested-action");
    generate.add_css_class("pill");
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("pill");
    cancel.set_visible(false);
    controls.append(&generate);
    controls.append(&cancel);
    controls.append(&preferences);

    let progress = gtk::ProgressBar::new();
    progress.set_valign(gtk::Align::Center);
    progress.set_hexpand(true);
    progress.set_visible(false);
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("dim-label");
    status.set_visible(false);
    let progress_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    progress_box.append(&progress);
    progress_box.append(&status);
    progress_box.set_visible(false);

    let selection_toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    selection_toolbar.set_visible(false);
    let selection_label = gtk::Label::new(Some("0 selected"));
    selection_label.add_css_class("dim-label");
    let select_all = gtk::Button::with_label("Select all");
    select_all.add_css_class("flat");
    let clear_selection = gtk::Button::with_label("Clear");
    clear_selection.add_css_class("flat");
    let add_selected = gtk::Button::with_label("Add selected to playlist");
    add_selected.add_css_class("suggested-action");
    add_selected.add_css_class("pill");
    selection_toolbar.append(&selection_label);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    selection_toolbar.append(&spacer);
    selection_toolbar.append(&select_all);
    selection_toolbar.append(&clear_selection);
    selection_toolbar.append(&add_selected);

    let results_group = adw::PreferencesGroup::builder()
        .title("Your picks")
        .description("Curated from artists in your lane")
        .visible(false)
        .build();

    let empty = adw::StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title(EMPTY_TITLE)
        .description(EMPTY_DESCRIPTION)
        .build();

    content.append(&hero);
    content.append(&summary);
    content.append(&controls);
    content.append(&progress_box);
    content.append(&selection_toolbar);
    content.append(&results_group);
    content.append(&empty);

    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content)
        .build();
    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(940);
    clamp.set_child(Some(&scroll));
    stack.add_titled_with_icon(&clamp, Some("list"), "List", "view-list-symbolic");

    // ---- Swipe page ----------------------------------------------------
    let swipe_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    swipe_page.set_vexpand(true);

    let swipe_empty = adw::StatusPage::builder()
        .icon_name("view-grid-symbolic")
        .title("Nothing to swipe yet")
        .description("Generate picks first, then keep the songs you like.")
        .build();

    let swipe_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    swipe_content.set_halign(gtk::Align::Center);
    swipe_content.set_valign(gtk::Align::Center);
    swipe_content.set_vexpand(true);
    swipe_content.set_margin_top(24);
    swipe_content.set_margin_bottom(24);
    swipe_content.set_margin_start(24);
    swipe_content.set_margin_end(24);

    let swipe_position = gtk::Label::new(None);
    swipe_position.set_halign(gtk::Align::Center);
    swipe_position.add_css_class("dim-label");
    swipe_position.add_css_class("caption");

    let swipe_card = gtk::Box::new(gtk::Orientation::Vertical, 14);
    swipe_card.set_width_request(560);
    swipe_card.set_margin_top(24);
    swipe_card.set_margin_bottom(24);
    swipe_card.set_margin_start(28);
    swipe_card.set_margin_end(28);
    swipe_card.add_css_class("card");
    swipe_card.add_css_class("background");

    let swipe_source = gtk::Label::new(None);
    swipe_source.set_halign(gtk::Align::Center);
    swipe_source.set_margin_top(18);
    swipe_source.add_css_class("dim-label");
    swipe_source.add_css_class("caption");
    let swipe_artist = gtk::Label::new(None);
    swipe_artist.set_halign(gtk::Align::Center);
    swipe_artist.set_wrap(true);
    swipe_artist.set_justify(gtk::Justification::Center);
    swipe_artist.set_margin_start(20);
    swipe_artist.set_margin_end(20);
    swipe_artist.add_css_class("title-1");
    let swipe_title = gtk::Label::new(None);
    swipe_title.set_halign(gtk::Align::Center);
    swipe_title.set_wrap(true);
    swipe_title.set_justify(gtk::Justification::Center);
    swipe_title.set_margin_start(20);
    swipe_title.set_margin_end(20);
    swipe_title.add_css_class("title-2");
    let swipe_meta = gtk::Label::new(None);
    swipe_meta.set_halign(gtk::Align::Center);
    swipe_meta.set_wrap(true);
    swipe_meta.set_margin_start(20);
    swipe_meta.set_margin_end(20);
    swipe_meta.add_css_class("dim-label");
    let swipe_reason = gtk::Label::new(None);
    swipe_reason.set_halign(gtk::Align::Center);
    swipe_reason.set_wrap(true);
    swipe_reason.set_justify(gtk::Justification::Center);
    swipe_reason.set_margin_top(6);
    swipe_reason.set_margin_start(20);
    swipe_reason.set_margin_end(20);
    let swipe_tags = gtk::Label::new(None);
    swipe_tags.set_halign(gtk::Align::Center);
    swipe_tags.set_wrap(true);
    swipe_tags.set_margin_start(20);
    swipe_tags.set_margin_end(20);
    swipe_tags.add_css_class("dim-label");
    swipe_tags.add_css_class("caption");

    let swipe_video_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    swipe_video_slot.set_halign(gtk::Align::Center);
    swipe_video_slot.set_size_request(460, 259);
    swipe_video_slot.set_margin_top(4);
    swipe_video_slot.set_margin_start(20);
    swipe_video_slot.set_margin_end(20);
    swipe_video_slot.set_visible(false);

    // Breathing room below the text, whether or not the video is shown.
    let swipe_bottom_pad = gtk::Box::new(gtk::Orientation::Vertical, 0);
    swipe_bottom_pad.set_height_request(20);

    swipe_card.append(&swipe_source);
    swipe_card.append(&swipe_artist);
    swipe_card.append(&swipe_title);
    swipe_card.append(&swipe_meta);
    swipe_card.append(&swipe_reason);
    swipe_card.append(&swipe_tags);
    swipe_card.append(&swipe_video_slot);
    swipe_card.append(&swipe_bottom_pad);

    let swipe_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    swipe_actions.set_halign(gtk::Align::Center);
    let swipe_previous = gtk::Button::from_icon_name("go-previous-symbolic");
    swipe_previous.add_css_class("flat");
    swipe_previous.set_tooltip_text(Some("Previous pick (Left arrow)"));
    let swipe_skip = gtk::Button::with_label("Skip");
    swipe_skip.add_css_class("pill");
    swipe_skip.set_tooltip_text(Some("Skip this pick (Left arrow)"));
    let swipe_add = gtk::Button::with_label("Add to playlist");
    swipe_add.add_css_class("suggested-action");
    swipe_add.add_css_class("pill");
    swipe_add.set_tooltip_text(Some("Add to playlist (Right arrow)"));
    let swipe_youtube = gtk::Button::with_label("YouTube");
    swipe_youtube.add_css_class("flat");
    swipe_youtube.set_tooltip_text(Some("Open a YouTube search"));
    swipe_actions.append(&swipe_previous);
    swipe_actions.append(&swipe_skip);
    swipe_actions.append(&swipe_add);
    swipe_actions.append(&swipe_youtube);

    let swipe_hint = gtk::Label::new(Some(
        "Swipe right to keep, left to skip · Arrow keys work too",
    ));
    swipe_hint.set_halign(gtk::Align::Center);
    swipe_hint.set_wrap(true);
    swipe_hint.add_css_class("dim-label");
    swipe_hint.add_css_class("caption");

    swipe_content.append(&swipe_position);
    swipe_content.append(&swipe_card);
    swipe_content.append(&swipe_actions);
    swipe_content.append(&swipe_hint);

    let swipe_done = adw::StatusPage::builder()
        .icon_name("object-select-symbolic")
        .title("You reviewed every pick")
        .description("Open your playlist to export it, or generate another batch.")
        .visible(false)
        .build();
    let done_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    done_actions.set_halign(gtk::Align::Center);
    let done_restart = gtk::Button::with_label("Back to first pick");
    done_restart.add_css_class("pill");
    let done_playlist = gtk::Button::with_label("Open playlist");
    done_playlist.add_css_class("suggested-action");
    done_playlist.add_css_class("pill");
    done_actions.append(&done_restart);
    done_actions.append(&done_playlist);
    swipe_done.set_child(Some(&done_actions));

    swipe_page.append(&swipe_empty);
    swipe_page.append(&swipe_content);
    swipe_page.append(&swipe_done);
    stack.add_titled_with_icon(&swipe_page, Some("swipe"), "Swipe", "view-grid-symbolic");

    // ---- Playlist page -------------------------------------------------
    let playlist_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
    playlist_box.set_margin_top(24);
    playlist_box.set_margin_bottom(24);
    playlist_box.set_margin_start(24);
    playlist_box.set_margin_end(24);

    let playlist_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let playlist_count = gtk::Label::new(Some("0 songs"));
    playlist_count.set_xalign(0.0);
    playlist_count.add_css_class("title-2");
    let playlist_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    playlist_spacer.set_hexpand(true);
    let export_csv_button = gtk::Button::with_label("Export CSV");
    export_csv_button.add_css_class("suggested-action");
    export_csv_button.add_css_class("pill");
    export_csv_button.set_tooltip_text(Some(
        "CSV with Track name / Artist name / Album name, for TuneMyMusic, Soundiiz and Spotify importers",
    ));
    let export_json_button = gtk::Button::with_label("JSON");
    export_json_button.add_css_class("pill");
    let export_m3u_button = gtk::Button::with_label("M3U");
    export_m3u_button.add_css_class("pill");
    let copy_playlist_button = gtk::Button::with_label("Copy");
    copy_playlist_button.add_css_class("flat");
    let playlist_clear = gtk::Button::from_icon_name("user-trash-symbolic");
    playlist_clear.add_css_class("flat");
    playlist_clear.set_tooltip_text(Some("Clear the playlist"));
    playlist_header.append(&playlist_count);
    playlist_header.append(&playlist_spacer);
    playlist_header.append(&export_csv_button);
    playlist_header.append(&export_json_button);
    playlist_header.append(&export_m3u_button);
    playlist_header.append(&copy_playlist_button);
    playlist_header.append(&playlist_clear);

    let playlist_note = gtk::Label::new(Some(
        "CSV works with TuneMyMusic, Soundiiz and most Spotify or YouTube Music importers. JSON keeps MusicBrainz IDs and search links; M3U opens in VLC or a browser.",
    ));
    playlist_note.set_xalign(0.0);
    playlist_note.set_wrap(true);
    playlist_note.add_css_class("dim-label");
    playlist_note.add_css_class("caption");

    let playlist_group = adw::PreferencesGroup::builder()
        .title("Accepted songs")
        .description("Only stored on this computer")
        .visible(false)
        .build();
    let playlist_empty = adw::StatusPage::builder()
        .icon_name("media-playlist-consecutive-symbolic")
        .title("Your playlist is empty")
        .description("Tick songs in List view, or keep them one by one in Swipe view.")
        .build();

    playlist_box.append(&playlist_header);
    playlist_box.append(&playlist_note);
    playlist_box.append(&playlist_group);
    playlist_box.append(&playlist_empty);

    let playlist_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&playlist_box)
        .build();
    let playlist_page = stack.add_titled_with_icon(
        &playlist_scroll,
        Some("playlist"),
        "Playlist",
        "media-playlist-consecutive-symbolic",
    );

    #[cfg(target_os = "linux")]
    let player = crate::player::PlayerView::new();
    #[cfg(target_os = "linux")]
    stack.add_titled_with_icon(
        player.widget(),
        Some("player"),
        "Player",
        "media-playback-start-symbolic",
    );

    stack.set_visible_child_name("list");

    toolbar_view.set_content(Some(&stack));

    let action_generate = gio::SimpleAction::new("generate", None);
    let action_prefs = gio::SimpleAction::new("preferences", None);
    let action_about = gio::SimpleAction::new("about", None);
    let action_export_json = gio::SimpleAction::new("export-json", None);
    let action_export_csv = gio::SimpleAction::new("export-csv", None);
    let action_copy = gio::SimpleAction::new("copy", None);
    let action_quit = gio::SimpleAction::new("quit", None);
    for action in [
        &action_generate,
        &action_prefs,
        &action_about,
        &action_export_json,
        &action_export_csv,
        &action_copy,
        &action_quit,
    ] {
        actions.add_action(action);
    }

    window.present();

    app.set_accels_for_action("win.generate", &["<Primary>g", "F5"]);
    app.set_accels_for_action("win.preferences", &["<Primary>comma"]);

    let ui = Ui {
        window,
        toast_overlay,
        stack,
        hero,
        summary,
        generate,
        cancel,
        preferences,
        preferences_header,
        progress,
        progress_box,
        status,
        empty,
        results_group,
        result_rows: RefCell::new(Vec::new()),
        result_checks: RefCell::new(Vec::new()),
        result_add_buttons: RefCell::new(Vec::new()),
        selection_toolbar,
        selection_label,
        select_all,
        clear_selection,
        add_selected,
        swipe_empty,
        swipe_content,
        swipe_done,
        swipe_position,
        swipe_source,
        swipe_artist,
        swipe_title,
        swipe_meta,
        swipe_reason,
        swipe_tags,
        swipe_video_slot,
        #[cfg(target_os = "linux")]
        swipe_player: RefCell::new(None),
        #[cfg(target_os = "linux")]
        swipe_video_mbid: RefCell::new(None),
        swipe_previous,
        swipe_skip,
        swipe_add,
        swipe_youtube,
        playlist_page,
        playlist_group,
        playlist_rows: RefCell::new(Vec::new()),
        playlist_empty,
        playlist_count,
        playlist_clear,
        playlist_export_csv: export_csv_button,
        playlist_export_json: export_json_button,
        playlist_export_m3u: export_m3u_button,
        playlist_copy: copy_playlist_button,
        done_restart,
        done_playlist,
        #[cfg(target_os = "linux")]
        player,
        #[cfg(target_os = "linux")]
        player_return: RefCell::new("list".to_string()),
        action_generate,
        action_export_json,
        action_export_csv,
        action_copy,
        preferences_action: action_prefs,
        about_action: action_about,
        quit_action: action_quit,
    };

    ui
}

fn connect_actions(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    // Re-fetch the buttons that are not part of Ui by looking them up from the
    // widget tree is brittle; they are connected in `build_ui` instead.
    // This function only wires actions and the stable controls.

    #[cfg(target_os = "linux")]
    {
        let ui_back = ui.clone();
        ui.player.connect_back(move || {
            let target = ui_back.player_return.borrow().clone();
            ui_back.stack.set_visible_child_name(&target);
        });
    }

    // Keep one stream alive at a time. Leaving the Player page or the swipe
    // page stops whichever video was playing there.
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.stack.connect_visible_child_name_notify(move |stack| {
            let name = stack
                .visible_child_name()
                .map(|name| name.to_string())
                .unwrap_or_default();
            #[cfg(target_os = "linux")]
            {
                if name != "player" {
                    ui_action.player.stop();
                }
                if name != "swipe" {
                    hide_swipe_video(&ui_action);
                } else {
                    maybe_autoplay_swipe_video(&ui_action, &state_owned);
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = name;
        });
    }

    let action_prefs = ui.preferences_action.clone();
    let action_about = ui.about_action.clone();
    let action_quit = ui.quit_action.clone();

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.generate.connect_clicked(move |_| {
            start_generation(&ui_action, &state_owned);
        });
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.cancel.connect_clicked(move |_| {
            if let Some(cancel) = state_owned.borrow().cancel.clone() {
                cancel.store(true, Ordering::SeqCst);
                ui_action.status.set_text("Cancelling…");
            }
        });
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.preferences.connect_clicked(move |_| {
            open_preferences(&ui_action, &state_owned);
        });
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.preferences_header.connect_clicked(move |_| {
            open_preferences(&ui_action, &state_owned);
        });
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        action_prefs.connect_activate(move |_, _| open_preferences(&ui_action, &state_owned));
    }

    {
        let ui_action = ui.clone();
        action_about.connect_activate(move |_, _| show_about(&ui_action));
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.action_generate
            .connect_activate(move |_, _| start_generation(&ui_action, &state_owned));
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.action_export_json
            .connect_activate(move |_, _| export_file(&ui_action, &state_owned, ExportKind::Json));
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.action_export_csv
            .connect_activate(move |_, _| export_file(&ui_action, &state_owned, ExportKind::Csv));
    }

    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.action_copy
            .connect_activate(move |_, _| copy_list(&ui_action, &state_owned));
    }

    {
        let ui_action = ui.clone();
        action_quit.connect_activate(move |_, _| {
            if let Some(app) = ui_action.window.application() {
                app.quit();
            }
        });
    }

    // Swipe controls.
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.swipe_skip
            .connect_clicked(move |_| swipe_skip(&ui_action, &state_owned));
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.swipe_previous
            .connect_clicked(move |_| swipe_previous(&ui_action, &state_owned));
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.swipe_add
            .connect_clicked(move |_| swipe_keep(&ui_action, &state_owned));
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.swipe_youtube.connect_clicked(move |_| {
            #[cfg(target_os = "linux")]
            {
                if ui_action.swipe_video_slot.is_visible() {
                    hide_swipe_video(&ui_action);
                } else if let Some(recommendation) = current_recommendation(&state_owned) {
                    show_swipe_video(&ui_action, &recommendation);
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                if let Some(recommendation) = current_recommendation(&state_owned) {
                    open_url(&ui_action, &recommendation.youtube_url);
                }
            }
        });
    }

    // Swipe gesture on the card.
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        let gesture = gtk::GestureSwipe::new();
        gesture.connect_swipe(move |_, velocity_x, _| {
            if velocity_x > 260.0 {
                swipe_keep(&ui_action, &state_owned);
            } else if velocity_x < -260.0 {
                swipe_skip(&ui_action, &state_owned);
            }
        });
        ui.swipe_content.add_controller(gesture);
    }

    // Arrow keys work while the swipe page is visible.
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if ui_action.stack.visible_child_name().as_deref() != Some("swipe") {
                return glib::Propagation::Proceed;
            }
            match keyval {
                gtk::gdk::Key::Left => {
                    swipe_skip(&ui_action, &state_owned);
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Right => {
                    swipe_keep(&ui_action, &state_owned);
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::BackSpace => {
                    swipe_previous(&ui_action, &state_owned);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        ui.window.add_controller(key);
    }

    // Playlist actions.
    // List selection controls.
    {
        let ui_action = ui.clone();
        ui.select_all.connect_clicked(move |_| {
            for check in ui_action.result_checks.borrow().iter() {
                check.set_active(true);
            }
            update_selection_label(&ui_action);
        });
    }
    {
        let ui_action = ui.clone();
        ui.clear_selection.connect_clicked(move |_| {
            for check in ui_action.result_checks.borrow().iter() {
                check.set_active(false);
            }
            update_selection_label(&ui_action);
        });
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.add_selected.connect_clicked(move |_| {
            let recommendations: Vec<Recommendation> = {
                let st = state_owned.borrow();
                let checks = ui_action.result_checks.borrow();
                match st.result.as_ref() {
                    Some(result) => checks
                        .iter()
                        .enumerate()
                        .filter(|(_, check)| check.is_active())
                        .filter_map(|(index, _)| result.recommendations.get(index).cloned())
                        .collect(),
                    None => Vec::new(),
                }
            };
            add_tracks(&ui_action, &state_owned, &recommendations);
            for check in ui_action.result_checks.borrow().iter() {
                check.set_active(false);
            }
            update_selection_label(&ui_action);
        });
    }

    // Playlist page controls.
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.playlist_export_csv.connect_clicked(move |_| {
            export_playlist(&ui_action, &state_owned, PlaylistExport::Csv);
        });
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.playlist_export_json.connect_clicked(move |_| {
            export_playlist(&ui_action, &state_owned, PlaylistExport::Json);
        });
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.playlist_export_m3u.connect_clicked(move |_| {
            export_playlist(&ui_action, &state_owned, PlaylistExport::M3u);
        });
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.playlist_copy
            .connect_clicked(move |_| copy_playlist(&ui_action, &state_owned));
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.playlist_clear
            .connect_clicked(move |_| clear_playlist(&ui_action, &state_owned));
    }
    {
        let ui_action = ui.clone();
        let state_owned = Rc::clone(state);
        ui.done_restart.connect_clicked(move |_| {
            hide_swipe_video(&ui_action);
            state_owned.borrow_mut().swipe_index = 0;
            update_swipe(&ui_action, &state_owned);
        });
    }
    {
        let ui_action = ui.clone();
        ui.done_playlist.connect_clicked(move |_| {
            ui_action.stack.set_visible_child_name("playlist");
        });
    }
}

fn start_generation(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    if state.borrow().running {
        return;
    }
    if let Err(error) = state.borrow().config.validate() {
        toast(ui, &error);
        open_preferences(ui, state);
        return;
    }

    ui.stack.set_visible_child_name("list");
    hide_swipe_video(ui);
    let cfg = state.borrow().config.clone();
    ui.status.set_text("Starting…");
    ui.progress.set_fraction(0.02);
    set_running(ui, state, true);

    let (tx, rx) = async_channel::bounded(64);
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_thread = cancel.clone();
    state.borrow_mut().cancel = Some(cancel);

    std::thread::spawn(move || {
        if let Ok(runtime) = tokio::runtime::Runtime::new() {
            let _ = runtime.block_on(pipeline::generate(cfg, tx, cancel_for_thread));
        }
    });

    let ui_loop = ui.clone();
    let s = state.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(message) = rx.recv().await {
            handle_progress(&ui_loop, &s, message);
        }
    });
}

fn handle_progress(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>, message: Progress) {
    match message {
        Progress::Stage { fraction, text } => {
            ui.progress.set_fraction(fraction.clamp(0.0, 1.0));
            ui.status.set_text(&text);
        }
        Progress::Done(result) => {
            {
                let mut s = state.borrow_mut();
                s.result = Some(*result);
                s.swipe_index = 0;
            }
            hide_swipe_video(ui);
            render_results(ui, state);
            set_running(ui, state, false);
            ui.status.set_text("Ready");
            ui.progress.set_fraction(1.0);
        }
        Progress::Failed(error) => {
            ui.status
                .set_text(&format!("Could not generate picks: {error}"));
            toast(ui, &error);
            set_running(ui, state, false);
        }
        Progress::Cancelled => {
            ui.status.set_text("Cancelled");
            toast(ui, "Generation cancelled");
            set_running(ui, state, false);
        }
    }
}

fn set_running(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>, running: bool) {
    state.borrow_mut().running = running;
    ui.generate.set_sensitive(!running);
    ui.cancel.set_visible(running);
    ui.progress.set_visible(running);
    // Keep the last status line around after a run so "Ready", "Cancelled" or
    // an error is still readable.
    let has_status = !ui.status.text().is_empty();
    ui.status.set_visible(has_status);
    ui.progress_box.set_visible(running || has_status);
    let has_result = state.borrow().result.is_some();
    ui.action_export_json.set_enabled(!running && has_result);
    ui.action_export_csv.set_enabled(!running && has_result);
    ui.action_copy.set_enabled(!running && has_result);
}

fn render_results(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let Some(result) = state.borrow().result.clone() else {
        return;
    };

    // Remove only the rows this app added; the group also owns a header box
    // that must not be touched.
    let previous = std::mem::take(&mut *ui.result_rows.borrow_mut());
    for row in previous {
        ui.results_group.remove(&row);
    }
    ui.result_checks.borrow_mut().clear();
    ui.result_add_buttons.borrow_mut().clear();

    ui.results_group
        .set_title(&format!("Your {} picks", result.recommendations.len()));
    let by = match &result.summary.ai_model {
        Some(model) => format!("curated by {model}"),
        None => "curated without AI".to_string(),
    };
    ui.results_group.set_description(Some(&format!(
        "{} · {} · tick songs to add them to your playlist",
        human_date(&result.summary.generated_at),
        by
    )));
    ui.results_group.set_visible(true);
    ui.empty.set_visible(false);
    ui.selection_toolbar.set_visible(true);
    ui.hero.set_text(&format!(
        "{} picks for {}",
        result.recommendations.len(),
        result.summary.username
    ));

    for recommendation in &result.recommendations {
        let row = adw::ActionRow::new();
        // Artists, titles and albums are plain text: a name like "A&B" must
        // not be parsed as Pango markup.
        row.set_use_markup(false);
        let check = gtk::CheckButton::new();
        check.set_valign(gtk::Align::Center);
        check.set_tooltip_text(Some("Add this song to the playlist"));
        let ui_for_check = ui.clone();
        check.connect_toggled(move |_| update_selection_label(&ui_for_check));
        let rank = gtk::Label::new(Some(&recommendation.rank.to_string()));
        rank.add_css_class("dim-label");
        rank.add_css_class("numeric");
        row.add_prefix(&check);
        row.add_prefix(&rank);
        row.set_title(&format!(
            "{} — {}",
            recommendation.artist, recommendation.title
        ));

        let album = recommendation
            .album
            .clone()
            .unwrap_or_else(|| "Unknown album".to_string());
        let subtitle = match recommendation.year {
            Some(year) => format!("{album} · {year}\n{}", recommendation.reason),
            None => format!("{album}\n{}", recommendation.reason),
        };
        row.set_subtitle(&subtitle);
        row.set_subtitle_lines(0);

        let ui_for_add = ui.clone();
        let state_for_add = state.clone();
        let recommendation_for_add = recommendation.clone();
        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.add_css_class("flat");
        add_button.set_tooltip_text(Some("Add to playlist"));
        add_button.connect_clicked(move |_| {
            add_tracks(
                &ui_for_add,
                &state_for_add,
                std::slice::from_ref(&recommendation_for_add),
            );
        });

        let ui_for_link = ui.clone();
        let url = recommendation.musicbrainz_url.clone();
        let mb_button = gtk::Button::from_icon_name("edit-find-symbolic");
        mb_button.add_css_class("flat");
        mb_button.set_tooltip_text(Some("Open on MusicBrainz"));
        mb_button.connect_clicked(move |_| open_url(&ui_for_link, &url));

        let ui_for_link = ui.clone();
        let recommendation_for_yt = recommendation.clone();
        let yt_button = gtk::Button::from_icon_name("media-playback-start-symbolic");
        yt_button.add_css_class("flat");
        yt_button.set_tooltip_text(Some("Play on YouTube"));
        yt_button.connect_clicked(move |_| {
            play_on_youtube(&ui_for_link, &recommendation_for_yt);
        });

        row.add_suffix(&add_button);
        row.add_suffix(&mb_button);
        row.add_suffix(&yt_button);
        ui.result_checks.borrow_mut().push(check);
        ui.result_add_buttons
            .borrow_mut()
            .push((recommendation.mbid.clone(), add_button));
        ui.result_rows.borrow_mut().push(row.clone());
        ui.results_group.add(&row);
    }

    refresh_add_buttons(ui, state);
    update_selection_label(ui);
    update_swipe(ui, state);
    update_summary(ui, state);
}

fn refresh_add_buttons(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let buttons = ui.result_add_buttons.borrow().clone();
    let playlist = state.borrow().playlist.clone();
    for (mbid, button) in buttons {
        if playlist.contains(&mbid) {
            button.set_icon_name("object-select-symbolic");
            button.set_tooltip_text(Some("Already in your playlist"));
            button.set_sensitive(false);
        } else {
            button.set_icon_name("list-add-symbolic");
            button.set_tooltip_text(Some("Add to playlist"));
            button.set_sensitive(true);
        }
    }
}

fn update_selection_label(ui: &Rc<Ui>) {
    let selected = ui
        .result_checks
        .borrow()
        .iter()
        .filter(|check| check.is_active())
        .count();
    ui.selection_label.set_text(&format!("{selected} selected"));
}

fn add_tracks(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>, recommendations: &[Recommendation]) {
    if recommendations.is_empty() {
        toast(ui, "Select at least one song first");
        return;
    }

    let mut added = 0usize;
    let save_error = {
        let mut st = state.borrow_mut();
        for recommendation in recommendations {
            if st.playlist.add(recommendation) {
                added += 1;
            }
        }
        if added > 0 {
            st.playlist.save().err().map(|error| error.to_string())
        } else {
            None
        }
    };

    if let Some(error) = save_error {
        toast(ui, &format!("Could not save your playlist: {error}"));
    }
    if added == 0 {
        toast(ui, "Already in your playlist");
        return;
    }

    render_playlist(ui, state);
    toast(
        ui,
        &format!(
            "Added {} song{} to your playlist",
            added,
            if added == 1 { "" } else { "s" }
        ),
    );
}

fn render_playlist(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let tracks = state.borrow().playlist.tracks.clone();
    let previous = std::mem::take(&mut *ui.playlist_rows.borrow_mut());
    for row in previous {
        ui.playlist_group.remove(&row);
    }

    let count = tracks.len();
    ui.playlist_count.set_text(&format!(
        "{} song{}",
        count,
        if count == 1 { "" } else { "s" }
    ));
    ui.playlist_page
        .set_badge_number(count.min(u32::MAX as usize) as u32);
    ui.playlist_clear.set_sensitive(count > 0);
    ui.playlist_empty.set_visible(count == 0);
    ui.playlist_group.set_visible(count > 0);
    ui.playlist_group.set_title("Accepted songs");
    ui.playlist_group.set_description(Some(&format!(
        "{count} song{} · only stored on this computer",
        if count == 1 { "" } else { "s" }
    )));

    for (index, track) in tracks.iter().enumerate() {
        let row = adw::ActionRow::new();
        row.set_use_markup(false);
        let number = gtk::Label::new(Some(&(index + 1).to_string()));
        number.add_css_class("dim-label");
        number.add_css_class("numeric");
        row.add_prefix(&number);
        row.set_title(&format!("{} — {}", track.artist, track.title));

        let mut details = Vec::new();
        if let Some(album) = track
            .album
            .as_deref()
            .map(str::trim)
            .filter(|album| !album.is_empty())
        {
            details.push(album.to_string());
        }
        if let Some(year) = track.year {
            details.push(year.to_string());
        }
        if !track.reason.trim().is_empty() {
            details.push(track.reason.clone());
        }
        row.set_subtitle(&details.join(" · "));
        row.set_subtitle_lines(0);

        let ui_for_open = ui.clone();
        let url = track.youtube_music_url();
        let open_button = gtk::Button::from_icon_name("media-playlist-consecutive-symbolic");
        open_button.add_css_class("flat");
        open_button.set_tooltip_text(Some("Search on YouTube Music"));
        open_button.connect_clicked(move |_| open_url(&ui_for_open, &url));

        let ui_for_remove = ui.clone();
        let state_for_remove = state.clone();
        let key = track.key();
        let remove_button = gtk::Button::from_icon_name("user-trash-symbolic");
        remove_button.add_css_class("flat");
        remove_button.set_tooltip_text(Some("Remove from playlist"));
        remove_button.connect_clicked(move |_| {
            let removed = {
                let mut st = state_for_remove.borrow_mut();
                let removed = st.playlist.remove(&key);
                if removed {
                    st.playlist.save().ok();
                }
                removed
            };
            if removed {
                render_playlist(&ui_for_remove, &state_for_remove);
            }
        });

        row.add_suffix(&open_button);
        row.add_suffix(&remove_button);
        ui.playlist_rows.borrow_mut().push(row.clone());
        ui.playlist_group.add(&row);
    }

    update_swipe(ui, state);
    refresh_add_buttons(ui, state);
}

fn current_recommendation(state: &Rc<RefCell<AppState>>) -> Option<Recommendation> {
    let st = state.borrow();
    let result = st.result.as_ref()?;
    result.recommendations.get(st.swipe_index).cloned()
}

fn update_swipe(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let (result, index, playlist_count) = {
        let st = state.borrow();
        (st.result.clone(), st.swipe_index, st.playlist.len())
    };

    let Some(result) = result else {
        hide_swipe_video(ui);
        ui.swipe_empty.set_visible(true);
        ui.swipe_content.set_visible(false);
        ui.swipe_done.set_visible(false);
        return;
    };

    if result.recommendations.is_empty() {
        hide_swipe_video(ui);
        ui.swipe_empty.set_visible(true);
        ui.swipe_content.set_visible(false);
        ui.swipe_done.set_visible(false);
        return;
    }

    if index >= result.recommendations.len() {
        hide_swipe_video(ui);
        ui.swipe_empty.set_visible(false);
        ui.swipe_content.set_visible(false);
        ui.swipe_done.set_visible(true);
        ui.swipe_done.set_description(Some(&format!(
            "You looked at all {} picks and kept {} song{}. Open your playlist to export it, or generate a new batch.",
            result.recommendations.len(),
            playlist_count,
            if playlist_count == 1 { "" } else { "s" }
        )));
        return;
    }

    let recommendation = &result.recommendations[index];
    ui.swipe_empty.set_visible(false);
    ui.swipe_done.set_visible(false);
    ui.swipe_content.set_visible(true);
    ui.swipe_position.set_text(&format!(
        "Pick {} of {} · {} in playlist",
        index + 1,
        result.recommendations.len(),
        playlist_count
    ));
    ui.swipe_source.set_text(recommendation.source.label());
    ui.swipe_artist.set_text(&recommendation.artist);
    ui.swipe_title.set_text(&recommendation.title);

    let mut meta = Vec::new();
    if let Some(album) = recommendation
        .album
        .as_deref()
        .map(str::trim)
        .filter(|album| !album.is_empty())
    {
        meta.push(album.to_string());
    }
    if let Some(year) = recommendation.year {
        meta.push(year.to_string());
    }
    ui.swipe_meta.set_text(&if meta.is_empty() {
        "Single".to_string()
    } else {
        meta.join(" · ")
    });
    ui.swipe_reason.set_text(&recommendation.reason);
    if recommendation.tags.is_empty() {
        ui.swipe_tags.set_visible(false);
    } else {
        ui.swipe_tags.set_visible(true);
        ui.swipe_tags
            .set_text(&format!("Tags: {}", recommendation.tags.join(", ")));
    }

    let in_playlist = state.borrow().playlist.contains(&recommendation.mbid);
    ui.swipe_add.set_sensitive(!in_playlist);
    ui.swipe_add.set_label(if in_playlist {
        "In playlist"
    } else {
        "Add to playlist"
    });
    ui.swipe_previous.set_sensitive(index > 0);
    ui.swipe_skip
        .set_label(if index + 1 == result.recommendations.len() {
            "Finish"
        } else {
            "Skip"
        });

    #[cfg(target_os = "linux")]
    maybe_autoplay_swipe_video(ui, state);
}

fn swipe_skip(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    hide_swipe_video(ui);
    let should_advance = {
        let st = state.borrow();
        st.result
            .as_ref()
            .is_some_and(|result| st.swipe_index < result.recommendations.len())
    };
    if should_advance {
        state.borrow_mut().swipe_index += 1;
        update_swipe(ui, state);
    }
}

fn swipe_previous(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    hide_swipe_video(ui);
    {
        let mut st = state.borrow_mut();
        st.swipe_index = st.swipe_index.saturating_sub(1);
    }
    update_swipe(ui, state);
}

fn swipe_keep(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let Some(recommendation) = current_recommendation(state) else {
        return;
    };
    hide_swipe_video(ui);
    add_tracks(ui, state, std::slice::from_ref(&recommendation));
    // A kept song moves the card forward, just like a skip does.
    let should_advance = {
        let st = state.borrow();
        st.result
            .as_ref()
            .is_some_and(|result| st.swipe_index < result.recommendations.len())
    };
    if should_advance {
        state.borrow_mut().swipe_index += 1;
        update_swipe(ui, state);
    }
}

fn update_summary(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let config = state.borrow().config.clone();
    let result = state.borrow().result.clone();

    ui.generate
        .set_label(&format!("Generate {} picks", config.discovery.count));

    if let Some(result) = result {
        let summary = &result.summary;
        let mut lines = vec![format!(
            "{} · {} listens",
            summary.username,
            format_number(summary.listen_count)
        )];
        if !summary.top_artists.is_empty() {
            lines.push(format!("Top artists: {}", summary.top_artists.join(", ")));
        }
        if !summary.top_genres.is_empty() {
            lines.push(format!("Top genres: {}", summary.top_genres.join(", ")));
        }
        lines.push(match &summary.ai_model {
            Some(model) => format!("AI curator: {model}"),
            None => "AI curator: off (deterministic picks)".to_string(),
        });
        if !summary.warnings.is_empty() {
            lines.push(format!("Note: {}", summary.warnings.join(" · ")));
        }
        ui.summary.set_text(&lines.join("\n"));
    } else if config.listenbrainz.username.trim().is_empty() {
        ui.hero.set_text(EMPTY_TITLE);
        ui.summary.set_text(EMPTY_DESCRIPTION);
    } else {
        let ai = if config.ai_ready() {
            format!("AI curator: {}", config.ai.model)
        } else {
            "AI curator: off (deterministic picks)".to_string()
        };
        let playlist = state.borrow().playlist.len();
        ui.summary.set_text(&format!(
            "ListenBrainz user {} · {} · {} song{} in your playlist · ready when you are",
            config.listenbrainz.username,
            ai,
            playlist,
            if playlist == 1 { "" } else { "s" }
        ));
    }
}

fn open_preferences(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let cfg = state.borrow().config.clone();
    let mode_index = DiscoveryMode::ALL
        .iter()
        .position(|mode| *mode == cfg.discovery.mode)
        .unwrap_or(0);
    let modes: Vec<&str> = DiscoveryMode::ALL.iter().map(|mode| mode.label()).collect();

    let page = adw::PreferencesPage::new();

    let lb_group = adw::PreferencesGroup::builder()
        .title("ListenBrainz")
        .description("Your listening history drives the candidates. A token is only needed for private profiles.")
        .build();
    let username_row = adw::EntryRow::new();
    username_row.set_title("Username");
    username_row.set_text(&cfg.listenbrainz.username);
    let token_row = adw::PasswordEntryRow::new();
    token_row.set_title("User token (optional)");
    token_row.set_text(&cfg.listenbrainz.token);
    lb_group.add(&username_row);
    lb_group.add(&token_row);

    let ai_group = adw::PreferencesGroup::builder()
        .title("AI curator (optional)")
        .description("Any OpenAI-compatible endpoint: OpenAI, OpenRouter, Groq, DeepSeek, Ollama, LM Studio. Off = deterministic picks.")
        .build();
    let ai_enabled = adw::SwitchRow::builder()
        .title("Curate with an AI model")
        .subtitle("The model only re-ranks real candidates, it cannot invent songs")
        .active(cfg.ai.enabled)
        .build();
    let base_row = adw::EntryRow::new();
    base_row.set_title("API base URL");
    base_row.set_text(&cfg.ai.base_url);
    let model_row = adw::EntryRow::new();
    model_row.set_title("Model");
    model_row.set_text(&cfg.ai.model);
    let key_row = adw::PasswordEntryRow::new();
    key_row.set_title("API key");
    key_row.set_text(&cfg.ai.api_key);
    ai_group.add(&ai_enabled);
    ai_group.add(&base_row);
    ai_group.add(&model_row);
    ai_group.add(&key_row);

    let disc_group = adw::PreferencesGroup::builder()
        .title("Discovery")
        .description("How far outside your library the picks should go.")
        .build();
    let count_row = adw::SpinRow::with_range(10.0, 100.0, 1.0);
    count_row.set_title("Number of picks");
    count_row.set_subtitle("10–100");
    count_row.set_value(cfg.discovery.count as f64);
    let mode_row = adw::ComboRow::builder()
        .title("Discovery mode")
        .subtitle("Balance of familiar artists and exploration")
        .model(&gtk::StringList::new(&modes))
        .build();
    mode_row.set_selected(mode_index as u32);
    let genre_row = adw::SwitchRow::builder()
        .title("Explore your top genres")
        .subtitle("Adds a genre-driven mix beyond similar artists")
        .active(cfg.discovery.include_genres)
        .build();
    let skip_row = adw::SwitchRow::builder()
        .title("Skip previously recommended songs")
        .subtitle("New runs leave out tracks this app picked before")
        .active(cfg.discovery.skip_recommended)
        .build();
    let repeats_row = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    repeats_row.set_title("Remember picks for (days)");
    repeats_row.set_subtitle("0 remembers every previous pick");
    repeats_row.set_value(cfg.discovery.avoid_repeats_days as f64);
    repeats_row.set_sensitive(skip_row.is_active());
    let repeats_for_switch = repeats_row.clone();
    skip_row.connect_active_notify(move |row| {
        repeats_for_switch.set_sensitive(row.is_active());
    });

    let history = config::History::load();
    let clear_row = adw::ActionRow::builder()
        .title("Recommendation history")
        .subtitle(format!("{} remembered pick(s)", history.len()))
        .build();
    let clear_button = gtk::Button::with_label("Clear");
    clear_button.add_css_class("flat");
    clear_button.set_valign(gtk::Align::Center);
    clear_row.add_suffix(&clear_button);
    clear_row.set_activatable_widget(Some(&clear_button));

    let ui_for_clear = ui.clone();
    let clear_row_reset = clear_row.clone();
    clear_button.connect_clicked(move |_| match config::History::load().clear() {
        Ok(()) => {
            clear_row_reset.set_subtitle("0 remembered picks");
            toast(&ui_for_clear, "Recommendation history cleared");
        }
        Err(error) => toast(
            &ui_for_clear,
            &format!("Could not clear recommendation history: {error}"),
        ),
    });

    disc_group.add(&count_row);
    disc_group.add(&mode_row);
    disc_group.add(&genre_row);
    disc_group.add(&skip_row);
    disc_group.add(&repeats_row);
    disc_group.add(&clear_row);

    page.add(&lb_group);
    page.add(&ai_group);
    page.add(&disc_group);

    let dialog = gtk::Window::builder()
        .title("lbreco Preferences")
        .modal(true)
        .default_width(620)
        .default_height(780)
        .build();
    dialog.set_transient_for(Some(&ui.window));
    dialog.set_destroy_with_parent(true);

    let header = gtk::HeaderBar::new();
    header.set_show_title_buttons(false);
    let cancel_button = gtk::Button::with_label("Cancel");
    let save_button = gtk::Button::with_label("Save");
    save_button.add_css_class("suggested-action");
    save_button.add_css_class("pill");
    header.pack_start(&cancel_button);
    header.pack_end(&save_button);
    dialog.set_titlebar(Some(&header));

    let page_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build();
    dialog.set_child(Some(&page_scroll));

    let dialog_for_cancel = dialog.clone();
    cancel_button.connect_clicked(move |_| dialog_for_cancel.close());

    let ui_save = ui.clone();
    let s = state.clone();
    let dialog_for_save = dialog.clone();
    save_button.connect_clicked(move |_| {
        let listenbrainz = ListenBrainzConfig {
            username: username_row.text().to_string(),
            token: token_row.text().to_string(),
        };
        let ai = AiConfig {
            enabled: ai_enabled.is_active(),
            base_url: base_row.text().to_string(),
            model: model_row.text().to_string(),
            api_key: key_row.text().to_string(),
        };
        let discovery = DiscoveryConfig {
            count: count_row.value().round().max(1.0) as usize,
            mode: mode_from_index(mode_row.selected()),
            include_genres: genre_row.is_active(),
            skip_recommended: skip_row.is_active(),
            avoid_repeats_days: repeats_row.value().round().max(0.0) as u32,
        };
        let new_config = Config {
            listenbrainz,
            ai,
            discovery,
        };
        if let Err(error) = new_config.validate() {
            toast(&ui_save, &error);
            return;
        }
        if let Err(error) = config::save(&new_config) {
            toast(&ui_save, &format!("Could not save preferences: {error}"));
            return;
        }
        s.borrow_mut().config = new_config;
        update_summary(&ui_save, &s);
        toast(&ui_save, "Preferences saved");
        dialog_for_save.close();
    });

    dialog.present();
}

fn show_about(ui: &Rc<Ui>) {
    let dialog = adw::AboutDialog::builder()
        .application_icon(APP_ID)
        .application_name("lbreco")
        .version(VERSION)
        .comments(EMPTY_DESCRIPTION)
        .developer_name("lbreco contributors")
        .website("https://listenbrainz.org")
        .license_type(gtk::License::MitX11)
        .build();
    dialog.present(Some(&ui.window));
}

fn export_file(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>, kind: ExportKind) {
    let Some(result) = state.borrow().result.clone() else {
        toast(ui, "Nothing to export yet");
        return;
    };
    let (extension, label, content) = match kind {
        ExportKind::Json => (
            "json",
            "Export picks as JSON",
            export::to_json(&result).unwrap_or_else(|error| {
                toast(ui, &format!("Could not build JSON: {error}"));
                String::new()
            }),
        ),
        ExportKind::Csv => ("csv", "Export picks as CSV", export::to_csv(&result)),
    };
    if content.is_empty() {
        return;
    }

    let date = chrono::Utc::now().format("%Y-%m-%d");
    save_text(
        ui,
        label,
        &format!("lbreco-picks-{date}.{extension}"),
        content,
    );
}

fn export_playlist(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>, kind: PlaylistExport) {
    let playlist = state.borrow().playlist.clone();
    if playlist.is_empty() {
        toast(ui, "Your playlist is empty");
        return;
    }

    let (label, extension, content) = match kind {
        PlaylistExport::Csv => ("Export playlist CSV", "csv", playlist::to_csv(&playlist)),
        PlaylistExport::Json => (
            "Export playlist JSON",
            "json",
            playlist::to_json(&playlist).unwrap_or_else(|error| {
                toast(ui, &format!("Could not build JSON: {error}"));
                String::new()
            }),
        ),
        PlaylistExport::M3u => ("Export playlist M3U", "m3u", playlist::to_m3u(&playlist)),
    };
    if content.is_empty() {
        return;
    }
    save_text(ui, label, &playlist::suggested_name(extension), content);
}

fn copy_playlist(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let playlist = state.borrow().playlist.clone();
    if playlist.is_empty() {
        toast(ui, "Your playlist is empty");
        return;
    }
    let text = playlist::to_plain_text(&playlist);
    ui.window.clipboard().set_text(&text);
    toast(ui, "Playlist copied as Artist - Title");
}

fn clear_playlist(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    if state.borrow().playlist.is_empty() {
        toast(ui, "Your playlist is already empty");
        return;
    }

    let dialog = adw::AlertDialog::builder()
        .heading("Clear your playlist?")
        .body("This removes every accepted song from lbreco. The exported files are not touched.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("clear", "Clear playlist");
    dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));

    let ui_clear = ui.clone();
    let state_clear = state.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "clear" {
            return;
        }
        let error = {
            let mut st = state_clear.borrow_mut();
            st.playlist.clear();
            st.playlist.save().err().map(|error| error.to_string())
        };
        render_playlist(&ui_clear, &state_clear);
        if let Some(error) = error {
            toast(&ui_clear, &format!("Could not save the playlist: {error}"));
        } else {
            toast(&ui_clear, "Playlist cleared");
        }
    });
    dialog.present(Some(&ui.window));
}

fn save_text(ui: &Rc<Ui>, label: &str, suggested_name: &str, content: String) {
    let dialog = gtk::FileDialog::builder()
        .title(label)
        .initial_name(suggested_name)
        .build();
    let window = ui.window.clone();
    let ui_toast = ui.clone();

    glib::MainContext::default().spawn_local(async move {
        match dialog.save_future(Some(&window)).await {
            Ok(file) => {
                let Some(path) = file.path() else {
                    toast(&ui_toast, "That location cannot be written to");
                    return;
                };
                if let Err(error) = std::fs::write(&path, content) {
                    toast(
                        &ui_toast,
                        &format!("Could not write {}: {error}", path.display()),
                    );
                } else {
                    toast(&ui_toast, "Export saved");
                }
            }
            Err(_) => {
                // The user dismissed the file dialog.
            }
        }
    });
}

fn copy_list(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    let Some(result) = state.borrow().result.clone() else {
        toast(ui, "Nothing to copy yet");
        return;
    };
    let text = export::to_plain_text(&result);
    ui.window.clipboard().set_text(&text);
    toast(ui, "Picks copied to the clipboard");
}

fn open_url(ui: &Rc<Ui>, url: &str) {
    if let Err(error) = gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>) {
        toast(ui, &format!("Could not open link: {error}"));
    }
}

fn play_on_youtube(ui: &Rc<Ui>, recommendation: &Recommendation) {
    let query = format!("{} {}", recommendation.artist, recommendation.title);
    let title = format!("{} — {}", recommendation.artist, recommendation.title);
    #[cfg(target_os = "linux")]
    {
        hide_swipe_video(ui);
        let previous = ui
            .stack
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_else(|| "list".to_string());
        if previous != "player" {
            *ui.player_return.borrow_mut() = previous;
        }
        ui.player.play(&title, &query);
        ui.stack.set_visible_child_name("player");
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (query, title);
        open_url(ui, &recommendation.youtube_url);
    }
}

#[cfg(target_os = "linux")]
fn ensure_swipe_player(ui: &Rc<Ui>) -> crate::player::PlayerView {
    if let Some(player) = ui.swipe_player.borrow().as_ref() {
        return player.clone();
    }

    let player = crate::player::PlayerView::new();
    player.set_bar_visible(false);
    ui.swipe_video_slot.append(player.widget());
    *ui.swipe_player.borrow_mut() = Some(player.clone());
    player
}

#[cfg(target_os = "linux")]
fn show_swipe_video(ui: &Rc<Ui>, recommendation: &Recommendation) {
    hide_swipe_video(ui);
    // The Player page shares the audio output; stop it before going inline.
    ui.player.stop();
    let player = ensure_swipe_player(ui);
    let title = format!("{} — {}", recommendation.artist, recommendation.title);
    let query = format!("{} {}", recommendation.artist, recommendation.title);
    // Make the slot visible first, so the placeholder is on screen while the
    // YouTube lookup runs instead of the card jumping later.
    ui.swipe_video_slot.set_visible(true);
    ui.swipe_youtube.set_label("Hide video");
    ui.swipe_youtube
        .set_tooltip_text(Some("Hide the video preview"));
    *ui.swipe_video_mbid.borrow_mut() = Some(recommendation.mbid.clone());
    player.play(&title, &query);
}

fn hide_swipe_video(ui: &Rc<Ui>) {
    ui.swipe_video_slot.set_visible(false);
    ui.swipe_youtube.set_label("YouTube");
    ui.swipe_youtube
        .set_tooltip_text(Some("Show the video preview"));
    #[cfg(target_os = "linux")]
    {
        *ui.swipe_video_mbid.borrow_mut() = None;
        if let Some(player) = ui.swipe_player.borrow().as_ref() {
            player.stop();
        }
    }
}

/// Show the current card's video as soon as the card is on screen, without
/// waiting for the YouTube button. A video already playing for this card is
/// left alone, so playlist refreshes do not restart it.
#[cfg(target_os = "linux")]
fn maybe_autoplay_swipe_video(ui: &Rc<Ui>, state: &Rc<RefCell<AppState>>) {
    if ui.stack.visible_child_name().as_deref() != Some("swipe") {
        return;
    }
    let current = current_recommendation(state);
    let shown = ui.swipe_video_mbid.borrow().clone();
    match current {
        Some(recommendation) if shown.as_deref() != Some(recommendation.mbid.as_str()) => {
            show_swipe_video(ui, &recommendation);
        }
        None => hide_swipe_video(ui),
        _ => {}
    }
}

fn toast(ui: &Rc<Ui>, text: &str) {
    ui.toast_overlay.add_toast(adw::Toast::new(text));
}

fn human_date(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}

fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::new();
    let mut count = 0;
    for ch in text.chars().rev() {
        if count > 0 && count % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
        count += 1;
    }
    out.chars().rev().collect()
}

fn mode_from_index(index: u32) -> DiscoveryMode {
    DiscoveryMode::ALL
        .get(index as usize)
        .copied()
        .unwrap_or(DiscoveryMode::Mixed)
}

#[derive(Clone, Copy)]
enum ExportKind {
    Json,
    Csv,
}

#[derive(Clone, Copy)]
enum PlaylistExport {
    Csv,
    Json,
    M3u,
}
