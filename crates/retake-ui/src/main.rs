//! Retake's window: Home, Library, Clip view, Montage editor and Settings. A separate process
//! from the recorder, started only while the window is open; it reads the clips folder and
//! the service's status file and sends commands over the control pipe. It draws on the CPU
//! (tiny-skia) and stops drawing while a game is in the foreground.

#![windows_subsystem = "windows"]

mod canvas;
mod data;
mod platform;
mod player;
mod settings;
mod style;

use std::path::{Path, PathBuf};
use std::time::Duration;

use data::{Clip, Montage, Prefs};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::image::Handle;
use iced::widget::{
    Space, button, checkbox, column, container, horizontal_space, image, mouse_area, pick_list,
    responsive, row, scrollable, text, text_input, toggler,
};
use iced::{Alignment, ContentFit, Element, Length, Subscription, Task};
use retake_core::control::{Command, Status};

fn main() -> iced::Result {
    let args = Args::parse();
    if !platform::first_instance() {
        return Ok(());
    }
    let icon = iced::window::icon::from_rgba(
        retake_core::logo::rgba(64, retake_core::logo::ACCENT, true),
        64,
        64,
    )
    .ok();
    iced::application(platform::TITLE, App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| style::theme())
        .font(include_bytes!("../assets/fonts/Inter-Regular.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/SpaceGrotesk-Medium.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/SpaceGrotesk-Bold.ttf").as_slice())
        .default_font(style::INTER)
        // RETAKE_UI_SCALE=1.5 or 2 shows the window at another display scaling (testing).
        .scale_factor(|_| {
            std::env::var("RETAKE_UI_SCALE")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(1.0)
        })
        .window(iced::window::Settings {
            size: iced::Size::new(1200.0, 780.0),
            min_size: Some(iced::Size::new(900.0, 620.0)),
            icon,
            ..Default::default()
        })
        .run_with(move || App::new(args))
}

#[derive(Debug, Clone)]
struct Args {
    config: PathBuf,
    status: PathBuf,
    /// Opens on this screen (`home`, `library`, `montage`, `settings`).
    screen: Option<String>,
    /// Opens this clip in the clip view.
    open: Option<PathBuf>,
}

impl Args {
    fn parse() -> Args {
        let mut config = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("config.toml")))
            .unwrap_or_else(|| PathBuf::from("config.toml"));
        let mut status = platform::app_data().join(retake_core::control::STATUS_FILE);
        let (mut screen, mut open) = (None, None);
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--config" => config = it.next().map_or(config, PathBuf::from),
                "--status" => status = it.next().map_or(status, PathBuf::from),
                "--screen" => screen = it.next(),
                "--open" => open = it.next().map(PathBuf::from),
                _ => {}
            }
        }
        Args {
            config,
            status,
            screen,
            open,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Library,
    Clip,
    Montage,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Since {
    All,
    Today,
    Week,
    Month,
}

impl std::fmt::Display for Since {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Since::All => "Any date",
            Since::Today => "Today",
            Since::Week => "Last 7 days",
            Since::Month => "Last 30 days",
        })
    }
}

/// A game filter choice: `None` = all games.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GameChoice(Option<String>);

impl std::fmt::Display for GameChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            None => f.write_str("All games"),
            Some(id) => f.write_str(&data::game_name(id)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Named2(String, String);

impl std::fmt::Display for Named2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TransitionChoice(retake_core::montage::Transitions);

impl std::fmt::Display for TransitionChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use retake_core::montage::Transitions as T;
        f.write_str(match self.0 {
            T::Cuts => "Cuts only",
            T::MostlyCuts => "Mostly cuts",
            T::Flashy => "Flashy",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InterpChoice(retake_core::montage::Interpolation);

impl std::fmt::Display for InterpChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use retake_core::montage::Interpolation as I;
        f.write_str(match self.0 {
            I::Auto => "Smooth slow-mo: auto",
            I::Always => "Smooth slow-mo: always",
            I::Off => "Smooth slow-mo: off",
        })
    }
}

/// The player's state (clip view and montage preview).
#[derive(Debug, Clone)]
struct Player {
    path: PathBuf,
    width: u32,
    height: u32,
    duration: f64,
    position: f64,
    frame: Option<Handle>,
    playing: bool,
    generation: u64,
    from: f64,
}

impl Player {
    fn run(&self) -> player::Run {
        player::Run {
            clip: self.path.clone(),
            from_ms: (self.from * 1000.0) as u64,
            width: self.width,
            height: self.height,
            generation: self.generation,
        }
    }
}

struct ClipView {
    clip: Clip,
    player: Option<Player>,
    trim: (f64, f64),
    add_to: Option<Named2>,
}

struct Editor {
    /// Where in the song the montage starts, seconds.
    song_start: f64,
    /// The whole song's peaks (the montage's part is cut from these for drawing).
    song_peaks: Vec<f32>,
    peaks: Vec<f32>,
    beats: Vec<(f64, bool)>,
    drop: Option<f64>,
    blocks: Vec<canvas::Block>,
    selected: Option<usize>,
    analysed: bool,
}

struct SettingsState {
    doc: toml_edit::DocumentMut,
    defaults: toml_edit::DocumentMut,
    query: String,
    message: Option<(bool, String)>,
    add_exe: String,
    dirty: bool,
}

struct App {
    args: Args,
    ffmpeg: Option<PathBuf>,
    status: Option<Status>,
    screen: Screen,
    clips: Vec<Clip>,
    filter_game: GameChoice,
    filter_starred: bool,
    filter_unused: bool,
    filter_since: Since,
    selected: usize,
    hover: Option<(PathBuf, Vec<Handle>, usize)>,
    view: Option<ClipView>,
    montages: Vec<Montage>,
    montage: Option<usize>,
    editor: Option<Editor>,
    preview: Option<Player>,
    new_song: String,
    new_youtube: bool,
    new_starred: bool,
    new_length: String,
    new_game: GameChoice,
    style_length: String,
    style_bpm: String,
    style_start: String,
    settings: SettingsState,
    prefs: Prefs,
    notice: Option<(String, i64)>,
    logo: Handle,
    quiet: bool,
    was_busy: bool,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Wake,
    Nav(Screen),
    Loaded(Vec<Clip>, Vec<Montage>),
    FilterGame(GameChoice),
    FilterStarred(bool),
    FilterUnused(bool),
    FilterSince(Since),
    HoverEnter(PathBuf),
    HoverExit,
    HoverStrip(PathBuf, Option<Vec<Handle>>),
    HoverTick,
    Open(PathBuf),
    Probed(PathBuf, Option<(u32, u32, f64)>, bool),
    Still(PathBuf, Option<Handle>),
    PlayPause,
    PreviewPlayPause,
    Player(player::Event),
    PreviewPlayer(player::Event),
    Timeline(canvas::TimelineMsg),
    Send(Command),
    Star,
    Trim,
    AddTo(Named2),
    AddToMontage,
    OpenFile(PathBuf),
    Reveal(PathBuf),
    Delete,
    Back,
    SelectMontage(usize),
    EditorLoaded(usize, Vec<f32>),
    MontageCanvas(canvas::MontageMsg),
    SongInput(String),
    BrowseSong,
    NewYoutube(bool),
    NewStarred(bool),
    NewLength(String),
    NewGame(GameChoice),
    CreateMontage,
    RenderMontage(bool),
    StyleYoutube(bool),
    StyleLength(String),
    StyleBpm(String),
    StyleStart(String),
    ApplyStyle,
    StyleSlowmo(f32),
    StyleBlur(f32),
    StyleShake(f32),
    StylePunch(f32),
    StyleTransitions(TransitionChoice),
    StyleInterpolation(InterpChoice),
    StyleMusic(bool),
    StyleKillfeed(bool),
    CancelRender,
    RemoveFromMontage,
    SettingsQuery(String),
    SettingToggle(usize, bool),
    SettingValue(usize, String),
    GameSwitch(String, &'static str, bool),
    AddExe(String),
    AddGame,
    SaveSettings,
    RevertSettings,
    ReduceMotion(bool),
    DismissIntro,
    StartService,
    Key(Key, Modifiers),
}

fn load_doc(path: &Path) -> toml_edit::DocumentMut {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_default()
}

fn load_all(clips_dir: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            (
                data::read_library(&clips_dir),
                data::read_montages(&clips_dir),
            )
        },
        |(c, m)| Message::Loaded(c, m),
    )
}

impl App {
    fn new(args: Args) -> (App, Task<Message>) {
        let status = data::read_status(&args.status);
        let doc = load_doc(&args.config);
        let app = App {
            ffmpeg: platform::ffmpeg(),
            status,
            screen: Screen::Home,
            clips: Vec::new(),
            filter_game: GameChoice(None),
            filter_starred: false,
            filter_unused: false,
            filter_since: Since::All,
            selected: 0,
            hover: None,
            view: None,
            montages: Vec::new(),
            montage: None,
            editor: None,
            preview: None,
            new_song: String::new(),
            new_youtube: false,
            new_starred: false,
            new_length: String::new(),
            new_game: GameChoice(None),
            style_length: String::new(),
            style_bpm: String::new(),
            style_start: String::new(),
            settings: SettingsState {
                doc,
                defaults: settings::defaults(),
                query: String::new(),
                message: None,
                add_exe: String::new(),
                dirty: false,
            },
            prefs: Prefs::load(),
            notice: None,
            logo: Handle::from_rgba(
                96,
                96,
                retake_core::logo::rgba(96, retake_core::logo::ACCENT, true),
            ),
            quiet: false,
            was_busy: false,
            args,
        };
        let mut app = app;
        app.screen = match app.args.screen.as_deref() {
            Some("library") => Screen::Library,
            Some("montage") => Screen::Montage,
            Some("settings") => Screen::Settings,
            _ => Screen::Home,
        };
        let mut task = load_all(app.clips_dir());
        if app.screen == Screen::Montage && !app.montages.is_empty() {
            task = Task::batch([task, app.load_editor(0)]);
        }
        (app, task)
    }

    fn clips_dir(&self) -> PathBuf {
        if let Some(s) = &self.status
            && !s.clips_dir.as_os_str().is_empty()
        {
            return s.clips_dir.clone();
        }
        // The service isn't running: read the folder from the config.
        let text = std::fs::read_to_string(&self.args.config).unwrap_or_default();
        retake_core::config::load_str(&text)
            .ok()
            .map(|c| c.clips.out_dir)
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from(r"C:\Retake\clips"))
    }

    fn live(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.live(data::unix_ms()))
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.notice = Some((msg.into(), data::unix_ms()));
    }

    fn send(&mut self, c: Command) {
        if let Err(e) = platform::send(&c) {
            self.notify(e);
        }
    }

    fn visible_clips(&self) -> Vec<&Clip> {
        let now = data::unix_ms() / 1000;
        let since = match self.filter_since {
            Since::All => i64::MIN,
            Since::Today => now - 86_400,
            Since::Week => now - 7 * 86_400,
            Since::Month => now - 30 * 86_400,
        };
        self.clips
            .iter()
            .filter(|c| self.filter_game.0.as_ref().is_none_or(|g| c.lib.game == *g))
            .filter(|c| !self.filter_starred || c.lib.starred)
            .filter(|c| !self.filter_unused || c.lib.used == 0)
            .filter(|c| c.lib.saved >= since)
            .collect()
    }

    fn games_in_library(&self) -> Vec<GameChoice> {
        let mut ids: Vec<String> = self.clips.iter().map(|c| c.lib.game.clone()).collect();
        ids.sort();
        ids.dedup();
        std::iter::once(GameChoice(None))
            .chain(ids.into_iter().map(|i| GameChoice(Some(i))))
            .collect()
    }

    fn open_clip(&mut self, path: PathBuf) -> Task<Message> {
        let Some(clip) = self.clips.iter().find(|c| c.lib.path == path).cloned() else {
            return Task::none();
        };
        let d = clip.lib.duration.max(0.1);
        self.view = Some(ClipView {
            clip,
            player: None,
            trim: (0.0, d),
            add_to: None,
        });
        self.screen = Screen::Clip;
        self.probe(path, false)
    }

    fn probe(&self, path: PathBuf, preview: bool) -> Task<Message> {
        let Some(ff) = self.ffmpeg.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                let p = player::probe(&ff, &path);
                (path, p)
            },
            move |(path, p)| Message::Probed(path, p, preview),
        )
    }

    fn still(&self, p: &Player) -> Task<Message> {
        let Some(ff) = self.ffmpeg.clone() else {
            return Task::none();
        };
        let (path, at, w, h) = (p.path.clone(), p.position, p.width, p.height);
        Task::perform(
            async move {
                let s = player::still(&ff, &path, at, w, h);
                (path, s)
            },
            |(path, s)| Message::Still(path, s),
        )
    }

    fn current_montage(&self) -> Option<&Montage> {
        self.montage.and_then(|i| self.montages.get(i))
    }

    /// Recomputes the montage timeline (arrangement, beats, drop) from the project and song.
    fn arrange_editor(&mut self, peaks: Option<Vec<f32>>) {
        let Some(m) = self.current_montage().cloned() else {
            self.editor = None;
            return;
        };
        let peaks_all = peaks.or_else(|| self.editor.as_ref().map(|e| e.song_peaks.clone()));
        let selected = self.editor.as_ref().and_then(|e| e.selected);
        let Some(mut a) = m.analysis() else {
            self.editor = Some(Editor {
                song_start: m.project.song_start.unwrap_or(0.0),
                song_peaks: peaks_all.unwrap_or_default(),
                peaks: Vec::new(),
                beats: Vec::new(),
                drop: None,
                blocks: Vec::new(),
                selected,
                analysed: false,
            });
            return;
        };
        if let Some(bpm) = m.project.bpm {
            a = a.respaced(bpm);
        }
        let p = &m.project;
        let start = p.song_start.unwrap_or_else(|| a.window_start(p.length));
        let placed = retake_core::montage::arrange(p, &a);
        let blocks = placed
            .iter()
            .filter_map(|x| {
                let index = p.clips.iter().position(|c| c.file == x.file)?;
                Some(canvas::Block {
                    index,
                    start: x.segs.first().map_or(0.0, |s| s.out0),
                    end: x.end(),
                    label: x
                        .file
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    kills: x.kills.iter().map(|k| k.1).collect(),
                    role: format!("{:?}", x.role).to_uppercase(),
                })
            })
            .collect();
        let bars: std::collections::HashSet<usize> = a.bars.iter().copied().collect();
        let beats = a
            .beats
            .iter()
            .enumerate()
            .map(|(i, b)| (b - start, bars.contains(&i)))
            .filter(|(t, _)| *t >= 0.0 && *t <= p.length)
            .collect();
        // The waveform over the montage's part of the song.
        let song_peaks = peaks_all.clone().unwrap_or_default();
        let peaks = peaks_all
            .map(|all| {
                if all.is_empty() || a.duration <= 0.0 {
                    return all;
                }
                let n = all.len() as f64;
                let i0 = ((start / a.duration) * n).floor().max(0.0) as usize;
                let i1 = (((start + p.length) / a.duration) * n).ceil().min(n) as usize;
                all.get(i0..i1.max(i0))
                    .map(<[f32]>::to_vec)
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.editor = Some(Editor {
            song_start: start,
            song_peaks,
            peaks,
            beats,
            drop: Some(a.drop_time() - start).filter(|d| *d >= 0.0 && *d <= p.length),
            blocks,
            selected,
            analysed: true,
        });
    }

    fn load_editor(&mut self, i: usize) -> Task<Message> {
        self.montage = Some(i);
        self.editor = None;
        self.preview = None;
        if let Some(p) = self.current_montage().map(|m| m.project.clone()) {
            self.style_length = format!("{:.0}", p.length);
            self.style_bpm = p.bpm.map(|b| format!("{b:.1}")).unwrap_or_default();
            self.style_start = p.song_start.map(|s| format!("{s:.1}")).unwrap_or_default();
        }
        self.arrange_editor(None);
        let (Some(ff), Some(m)) = (self.ffmpeg.clone(), self.current_montage().cloned()) else {
            return Task::none();
        };
        Task::perform(
            async move { data::waveform(&ff, &m.project.song, &m.dir.join("waveform.json"), 3000) },
            move |p| Message::EditorLoaded(i, p),
        )
    }

    fn edit_montage(&mut self, f: impl FnOnce(&mut retake_core::montage::Project)) {
        let Some(i) = self.montage else { return };
        let Some(m) = self.montages.get_mut(i) else {
            return;
        };
        f(&mut m.project);
        if let Err(e) = m.save() {
            self.notify(format!("Couldn't save the montage: {e}"));
        }
        self.arrange_editor(None);
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick | Message::Wake => {
                let s = data::read_status(&self.args.status);
                let busy = s.as_ref().is_some_and(|s| s.montage_busy);
                let queued_changed = s.as_ref().map(|s| (s.clips_today, s.queued))
                    != self.status.as_ref().map(|s| (s.clips_today, s.queued));
                self.quiet = s
                    .as_ref()
                    .is_some_and(|s| s.game_foreground && s.live(data::unix_ms()));
                self.status = s;
                if self
                    .notice
                    .as_ref()
                    .is_some_and(|(_, t)| data::unix_ms() - t > 6_000)
                {
                    self.notice = None;
                }
                let finished = self.was_busy && !busy;
                self.was_busy = busy;
                if self.quiet {
                    // A game has the focus: stop playback, draw nothing until woken.
                    if let Some(v) = self.view.as_mut().and_then(|v| v.player.as_mut()) {
                        v.playing = false;
                    }
                    if let Some(p) = &mut self.preview {
                        p.playing = false;
                    }
                }
                if finished || queued_changed {
                    let reload = load_all(self.clips_dir());
                    return reload;
                }
                Task::none()
            }
            Message::Nav(s) => {
                self.screen = s;
                self.hover = None;
                if s == Screen::Settings {
                    self.settings.doc = load_doc(&self.args.config);
                    self.settings.dirty = false;
                }
                if s == Screen::Library || s == Screen::Montage || s == Screen::Home {
                    return load_all(self.clips_dir());
                }
                Task::none()
            }
            Message::Loaded(c, m) => {
                let first_montage = self.montages.is_empty()
                    && self.screen == Screen::Montage
                    && self.montage.is_none();
                self.clips = c;
                if let Some(p) = self.args.open.take() {
                    self.montages = m;
                    return self.open_clip(p);
                }
                let keep = self.current_montage().map(|m| m.dir.clone());
                self.montages = m;
                self.montage = keep.and_then(|d| self.montages.iter().position(|m| m.dir == d));
                if let Some(v) = &mut self.view
                    && let Some(c) = self.clips.iter().find(|c| c.lib.path == v.clip.lib.path)
                {
                    v.clip = c.clone();
                }
                self.arrange_editor(None);
                if first_montage && !self.montages.is_empty() {
                    return self.load_editor(0);
                }
                // A finished preview plays in place.
                if let Some(m) = self.current_montage()
                    && self.preview.is_none()
                    && m.dir.join("preview.mp4").is_file()
                {
                    return self.probe(m.dir.join("preview.mp4"), true);
                }
                Task::none()
            }
            Message::FilterGame(g) => {
                self.filter_game = g;
                self.selected = 0;
                Task::none()
            }
            Message::FilterStarred(b) => {
                self.filter_starred = b;
                Task::none()
            }
            Message::FilterUnused(b) => {
                self.filter_unused = b;
                Task::none()
            }
            Message::FilterSince(s) => {
                self.filter_since = s;
                Task::none()
            }
            Message::HoverEnter(p) => {
                if self.prefs.reduce_motion {
                    return Task::none();
                }
                let strip = self
                    .clips
                    .iter()
                    .find(|c| c.lib.path == p)
                    .and_then(|c| c.strip.clone());
                let Some(strip) = strip else {
                    return Task::none();
                };
                self.hover = Some((p.clone(), Vec::new(), 0));
                Task::perform(async move { data::split_strip(&strip) }, move |f| {
                    Message::HoverStrip(p.clone(), f)
                })
            }
            Message::HoverStrip(p, frames) => {
                if let (Some(h), Some(f)) = (&mut self.hover, frames)
                    && h.0 == p
                {
                    h.1 = f;
                }
                Task::none()
            }
            Message::HoverExit => {
                self.hover = None;
                Task::none()
            }
            Message::HoverTick => {
                if let Some(h) = &mut self.hover
                    && !h.1.is_empty()
                {
                    h.2 = (h.2 + 1) % h.1.len();
                }
                Task::none()
            }
            Message::Open(p) => self.open_clip(p),
            Message::Probed(path, info, preview) => {
                let Some((w, h, duration)) = info else {
                    self.notify("Couldn't read the clip");
                    return Task::none();
                };
                let p = Player {
                    path,
                    width: player::WIDTH,
                    height: player::height_for(w, h),
                    duration,
                    position: 0.0,
                    frame: None,
                    playing: false,
                    generation: 0,
                    from: 0.0,
                };
                let t = self.still(&p);
                if preview {
                    self.preview = Some(p);
                } else if let Some(v) = &mut self.view
                    && v.clip.lib.path == p.path
                {
                    v.trim = (0.0, duration);
                    v.player = Some(p);
                }
                t
            }
            Message::Still(path, img) => {
                for p in [
                    self.view.as_mut().and_then(|v| v.player.as_mut()),
                    self.preview.as_mut(),
                ]
                .into_iter()
                .flatten()
                {
                    if p.path == path && !p.playing && img.is_some() {
                        p.frame = img.clone();
                    }
                }
                Task::none()
            }
            Message::PlayPause => {
                if let Some(p) = self.view.as_mut().and_then(|v| v.player.as_mut()) {
                    toggle_play(p);
                    if !p.playing {
                        let p = p.clone();
                        return self.still(&p);
                    }
                }
                Task::none()
            }
            Message::PreviewPlayPause => {
                if let Some(p) = &mut self.preview {
                    toggle_play(p);
                }
                Task::none()
            }
            Message::Player(e) => {
                if let Some(p) = self.view.as_mut().and_then(|v| v.player.as_mut()) {
                    on_player(p, e);
                }
                Task::none()
            }
            Message::PreviewPlayer(e) => {
                if let Some(p) = &mut self.preview {
                    on_player(p, e);
                }
                Task::none()
            }
            Message::Timeline(t) => {
                let Some(v) = &mut self.view else {
                    return Task::none();
                };
                match t {
                    canvas::TimelineMsg::Seek(at) => {
                        if let Some(p) = &mut v.player {
                            p.position = at;
                            p.from = at;
                            p.generation += 1;
                            if !p.playing {
                                let p = p.clone();
                                return self.still(&p);
                            }
                        }
                    }
                    canvas::TimelineMsg::TrimIn(at) => v.trim.0 = at.max(0.0),
                    canvas::TimelineMsg::TrimOut(at) => {
                        let d = v
                            .player
                            .as_ref()
                            .map_or(v.clip.lib.duration, |p| p.duration);
                        v.trim.1 = at.min(d);
                    }
                }
                Task::none()
            }
            Message::Send(c) => {
                let label = match &c {
                    Command::Export { preset, .. } => format!("Export ({preset}) queued"),
                    Command::Regrade { .. } => "Regrade queued".into(),
                    Command::Pause { on: true } => "Recording paused".into(),
                    Command::Pause { on: false } => "Recording resumed".into(),
                    Command::SaveFull => "Saving the last 60 s".into(),
                    Command::Rescan => "Rescanning the library".into(),
                    Command::CopyDiagnostics => "Diagnostics copied".into(),
                    _ => String::new(),
                };
                self.send(c);
                if !label.is_empty() && self.notice.is_none() {
                    self.notify(label);
                }
                Task::none()
            }
            Message::Star => {
                let Some(v) = &mut self.view else {
                    return Task::none();
                };
                let on = !v.clip.lib.starred;
                v.clip.lib.starred = on;
                let clip = v.clip.lib.path.clone();
                if let Some(c) = self.clips.iter_mut().find(|c| c.lib.path == clip) {
                    c.lib.starred = on;
                }
                self.send(Command::Star { clip, on });
                Task::none()
            }
            Message::Trim => {
                let Some(v) = &self.view else {
                    return Task::none();
                };
                let c = Command::Trim {
                    clip: v.clip.lib.path.clone(),
                    from: v.trim.0,
                    to: v.trim.1,
                };
                self.send(c);
                self.notify("Saving the trimmed clip");
                Task::none()
            }
            Message::AddTo(m) => {
                if let Some(v) = &mut self.view {
                    v.add_to = Some(m);
                }
                Task::none()
            }
            Message::AddToMontage => {
                let Some(v) = &self.view else {
                    return Task::none();
                };
                let (Some(target), clip) = (v.add_to.clone(), v.clip.clone()) else {
                    return Task::none();
                };
                let Some(i) = self
                    .montages
                    .iter()
                    .position(|m| m.dir.to_string_lossy() == target.0)
                else {
                    return Task::none();
                };
                let m = &mut self.montages[i];
                if m.project.clips.iter().any(|c| c.file == clip.lib.path) {
                    self.notify("Already in that montage");
                    return Task::none();
                }
                m.project.add_clip(&clip.lib);
                let r = m.save();
                match r {
                    Ok(()) => self.notify(format!("Added to {}", target.1)),
                    Err(e) => self.notify(format!("Couldn't add it: {e}")),
                }
                Task::none()
            }
            Message::OpenFile(p) => {
                platform::open(&p);
                Task::none()
            }
            Message::Reveal(p) => {
                platform::reveal(&p);
                Task::none()
            }
            Message::Delete => {
                let Some(v) = self.view.take() else {
                    return Task::none();
                };
                self.send(Command::Delete {
                    clip: v.clip.lib.path.clone(),
                });
                self.clips.retain(|c| c.lib.path != v.clip.lib.path);
                self.screen = Screen::Library;
                self.notify("Moved to the Recycle Bin");
                Task::none()
            }
            Message::Back => {
                self.view = None;
                self.screen = Screen::Library;
                Task::none()
            }
            Message::SelectMontage(i) => self.load_editor(i),
            Message::EditorLoaded(i, peaks) => {
                if self.montage == Some(i) {
                    self.arrange_editor(Some(peaks));
                }
                if let Some(m) = self.current_montage()
                    && m.dir.join("preview.mp4").is_file()
                {
                    return self.probe(m.dir.join("preview.mp4"), true);
                }
                Task::none()
            }
            Message::MontageCanvas(m) => {
                match m {
                    canvas::MontageMsg::Select(i) => {
                        if let Some(e) = &mut self.editor {
                            e.selected = Some(i);
                        }
                    }
                    canvas::MontageMsg::Move { from, to } => {
                        self.edit_montage(|p| {
                            let mut i = from;
                            while i != to {
                                let up = to < i;
                                let j = p.move_clip(i, up);
                                if j == i {
                                    break;
                                }
                                i = j;
                            }
                        });
                        if let Some(e) = &mut self.editor {
                            e.selected = Some(to);
                        }
                    }
                    canvas::MontageMsg::Resize { index, by } => {
                        let span = self
                            .editor
                            .as_ref()
                            .and_then(|e| e.blocks.iter().find(|b| b.index == index))
                            .map(|b| b.end - b.start);
                        self.edit_montage(|p| {
                            if let (Some(c), Some(span)) = (p.clips.get_mut(index), span) {
                                let (i, o) = (c.r#in.unwrap_or(0.0), c.out.unwrap_or(c.duration));
                                let speed = (o - i) / span.max(0.1);
                                c.out = Some((o + by * speed).clamp(i + 0.5, c.duration));
                                c.r#in = Some(i);
                                c.manual = true;
                            }
                        });
                    }
                }
                Task::none()
            }
            Message::SongInput(s) => {
                self.new_song = s;
                Task::none()
            }
            Message::BrowseSong => {
                if let Some(p) = platform::pick_song() {
                    self.new_song = p.to_string_lossy().into_owned();
                }
                Task::none()
            }
            Message::NewYoutube(b) => {
                self.new_youtube = b;
                Task::none()
            }
            Message::NewStarred(b) => {
                self.new_starred = b;
                Task::none()
            }
            Message::NewLength(s) => {
                self.new_length = s;
                Task::none()
            }
            Message::NewGame(g) => {
                self.new_game = g;
                Task::none()
            }
            Message::CreateMontage => {
                let song = PathBuf::from(self.new_song.trim().trim_matches('"'));
                if !song.is_file() {
                    self.notify("Pick a song first");
                    return Task::none();
                }
                let c = Command::MontageNew {
                    song,
                    youtube: self.new_youtube,
                    starred: self.new_starred,
                    length: self.new_length.trim().parse().ok(),
                    game: self.new_game.0.clone(),
                };
                self.send(c);
                self.notify("Making the montage and a preview…");
                Task::none()
            }
            Message::RenderMontage(preview) => {
                let Some(m) = self.current_montage() else {
                    return Task::none();
                };
                let c = Command::MontageRender {
                    dir: m.dir.clone(),
                    preview,
                };
                if preview {
                    self.preview = None;
                }
                self.send(c);
                self.notify(if preview {
                    "Rendering a preview…"
                } else {
                    "Rendering the montage…"
                });
                Task::none()
            }
            Message::StyleYoutube(b) => {
                self.edit_montage(|p| {
                    p.format = if b {
                        retake_core::montage::Format::Youtube
                    } else {
                        retake_core::montage::Format::Tiktok
                    };
                });
                Task::none()
            }
            Message::StyleLength(s) => {
                self.style_length = s;
                Task::none()
            }
            Message::StyleBpm(s) => {
                self.style_bpm = s;
                Task::none()
            }
            Message::StyleStart(s) => {
                self.style_start = s;
                Task::none()
            }
            Message::ApplyStyle => {
                let length = self.style_length.trim().parse::<f64>().ok();
                let bpm = self
                    .style_bpm
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|b| (40.0..=240.0).contains(b));
                let start = self
                    .style_start
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|s| *s >= 0.0);
                self.edit_montage(|p| {
                    if let Some(l) = length {
                        p.length = l.clamp(5.0, 600.0);
                    }
                    p.bpm = bpm;
                    p.song_start = start;
                });
                Task::none()
            }
            Message::StyleSlowmo(v) => self.style_edit(|s| s.slowmo = f64::from(v)),
            Message::StyleBlur(v) => self.style_edit(|s| s.blur = f64::from(v)),
            Message::StyleShake(v) => self.style_edit(|s| s.shake = f64::from(v)),
            Message::StylePunch(v) => self.style_edit(|s| s.punch = f64::from(v)),
            Message::StyleTransitions(c) => self.style_edit(|s| s.transitions = c.0),
            Message::StyleInterpolation(c) => self.style_edit(|s| s.interpolation = c.0),
            Message::StyleMusic(b) => self.style_edit(|s| s.music_in_video = b),
            Message::StyleKillfeed(b) => self.style_edit(|s| s.killfeed = b),
            Message::CancelRender => {
                self.send(Command::MontageCancel);
                Task::none()
            }
            Message::RemoveFromMontage => {
                let sel = self.editor.as_ref().and_then(|e| e.selected);
                if let Some(i) = sel {
                    self.edit_montage(|p| {
                        if let Some(c) = p.clips.get_mut(i) {
                            c.include = false;
                        }
                    });
                }
                Task::none()
            }
            Message::SettingsQuery(q) => {
                self.settings.query = q;
                Task::none()
            }
            Message::SettingToggle(i, on) => {
                self.set_field(i, if on { "true" } else { "false" });
                Task::none()
            }
            Message::SettingValue(i, v) => {
                self.set_field(i, &v);
                Task::none()
            }
            Message::GameSwitch(id, key, on) => {
                settings::set_game(&mut self.settings.doc, &id, key, on);
                self.settings.dirty = true;
                if key == "strict"
                    && !on
                    && retake_core::games::BUILTIN
                        .iter()
                        .any(|p| p.id == id && p.anticheat.strict_by_default())
                {
                    self.settings.message = Some((
                        false,
                        format!(
                            "{} uses a kernel-level anti-cheat. Strict safety off means game audio is captured by process id. Only do this if you're sure.",
                            data::game_name(&id)
                        ),
                    ));
                }
                Task::none()
            }
            Message::AddExe(s) => {
                self.settings.add_exe = s;
                Task::none()
            }
            Message::AddGame => {
                match settings::add_game(&mut self.settings.doc, &self.settings.add_exe) {
                    Ok(name) => {
                        self.settings.dirty = true;
                        self.settings.add_exe.clear();
                        self.settings.message = Some((
                            true,
                            format!("{name} added. Press Save to start recording it."),
                        ));
                    }
                    Err(e) => self.settings.message = Some((false, e)),
                }
                Task::none()
            }
            Message::SaveSettings => {
                self.settings.message = Some(
                    match settings::save(&self.args.config, &self.settings.doc) {
                        Ok(()) => {
                            self.settings.dirty = false;
                            (true, "Saved. Retake restarts with the new settings now, or after the match if a game is running.".into())
                        }
                        Err(e) => (false, format!("Not saved: {e}")),
                    },
                );
                Task::none()
            }
            Message::RevertSettings => {
                self.settings.doc = load_doc(&self.args.config);
                self.settings.dirty = false;
                self.settings.message = None;
                Task::none()
            }
            Message::ReduceMotion(b) => {
                self.prefs.reduce_motion = b;
                self.prefs.save();
                self.hover = None;
                Task::none()
            }
            Message::DismissIntro => {
                self.prefs.intro_seen = true;
                self.prefs.save();
                Task::none()
            }
            Message::StartService => {
                match platform::start_service() {
                    Ok(()) => self.notify("Starting Retake…"),
                    Err(e) => self.notify(e),
                }
                Task::none()
            }
            Message::Key(key, m) => self.on_key(key, m),
        }
    }

    /// Changes the montage's style and saves it (sliders save as they move).
    fn style_edit(&mut self, f: impl FnOnce(&mut retake_core::montage::Style)) -> Task<Message> {
        self.edit_montage(|p| {
            f(&mut p.style);
            p.style = p.style.clamped();
        });
        Task::none()
    }

    fn set_field(&mut self, i: usize, v: &str) {
        if let Some(fl) = settings::FIELDS.get(i) {
            match settings::set(&mut self.settings.doc, fl, v) {
                Ok(()) => self.settings.dirty = true,
                Err(e) => self.settings.message = Some((false, e)),
            }
        }
    }

    fn on_key(&mut self, key: Key, m: Modifiers) -> Task<Message> {
        if m.control() {
            let to = match key.as_ref() {
                Key::Character("1") => Some(Screen::Home),
                Key::Character("2") => Some(Screen::Library),
                Key::Character("3") => Some(Screen::Montage),
                Key::Character("4") => Some(Screen::Settings),
                _ => None,
            };
            return to.map_or(Task::none(), |s| self.update(Message::Nav(s)));
        }
        match self.screen {
            Screen::Library => {
                let n = self.visible_clips().len();
                if n == 0 {
                    return Task::none();
                }
                let cols = 3;
                match key.as_ref() {
                    Key::Named(Named::ArrowRight) => self.selected = (self.selected + 1).min(n - 1),
                    Key::Named(Named::ArrowLeft) => self.selected = self.selected.saturating_sub(1),
                    Key::Named(Named::ArrowDown) => {
                        self.selected = (self.selected + cols).min(n - 1)
                    }
                    Key::Named(Named::ArrowUp) => {
                        self.selected = self.selected.saturating_sub(cols)
                    }
                    Key::Named(Named::Enter) => {
                        let p = self.visible_clips()[self.selected.min(n - 1)]
                            .lib
                            .path
                            .clone();
                        return self.open_clip(p);
                    }
                    _ => {}
                }
                Task::none()
            }
            Screen::Clip => match key.as_ref() {
                Key::Named(Named::Space) => self.update(Message::PlayPause),
                Key::Named(Named::Escape) => self.update(Message::Back),
                Key::Named(Named::ArrowLeft | Named::ArrowRight) => {
                    let right = key == Key::Named(Named::ArrowRight);
                    let at = self.view.as_ref().and_then(|v| v.player.as_ref()).map(|p| {
                        (p.position + if right { 5.0 } else { -5.0 }).clamp(0.0, p.duration)
                    });
                    at.map_or(Task::none(), |a| {
                        self.update(Message::Timeline(canvas::TimelineMsg::Seek(a)))
                    })
                }
                Key::Character("s") => self.update(Message::Star),
                _ => Task::none(),
            },
            _ => Task::none(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![iced::keyboard::on_key_press(|k, m| {
            Some(Message::Key(k, m))
        })];
        if self.quiet {
            // A game has the focus: no timers; any input to the window wakes it.
            subs.push(iced::event::listen_with(|e, _, _| match e {
                iced::Event::Window(iced::window::Event::Focused)
                | iced::Event::Mouse(iced::mouse::Event::CursorEntered) => Some(Message::Wake),
                _ => None,
            }));
            return Subscription::batch(subs);
        }
        subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick));
        if let (Some(ff), Some(p)) = (
            &self.ffmpeg,
            self.view.as_ref().and_then(|v| v.player.as_ref()),
        ) && p.playing
        {
            let run = p.run();
            subs.push(
                Subscription::run_with_id(("clip", run.clone()), player::frames(ff.clone(), run))
                    .map(Message::Player),
            );
        }
        if let (Some(ff), Some(p)) = (&self.ffmpeg, &self.preview)
            && p.playing
            && self.screen == Screen::Montage
        {
            let run = p.run();
            subs.push(
                Subscription::run_with_id(
                    ("preview", run.clone()),
                    player::frames(ff.clone(), run),
                )
                .map(Message::PreviewPlayer),
            );
        }
        if self.hover.is_some() && !self.prefs.reduce_motion {
            subs.push(iced::time::every(Duration::from_millis(140)).map(|_| Message::HoverTick));
        }
        Subscription::batch(subs)
    }

    // ---- Views -------------------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let content: Element<'_, Message> = match self.screen {
            Screen::Home => self.home(),
            Screen::Library => self.library(),
            Screen::Clip => self.clip_view(),
            Screen::Montage => self.montage_view(),
            Screen::Settings => self.settings_view(),
        };
        let mut main = column![].spacing(12);
        if let Some((n, _)) = &self.notice {
            main = main.push(
                container(text(n.clone()).size(14))
                    .padding([13, 16])
                    .width(Length::Fill)
                    .style(style::banner(style::ACCENT)),
            );
        }
        main = main.push(content);
        let ui = row![
            self.sidebar(),
            container(main)
                .padding(24)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(style::page),
        ];
        // A full-window quad that changes on every view: the CPU renderer then repaints the
        // whole window (its damage tracking in iced 0.13 misses canvas and image changes).
        container(ui)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::repaint())
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let nav = |label: &'static str, s: Screen| {
            let active = self.screen == s || (s == Screen::Library && self.screen == Screen::Clip);
            button(text(label).size(15).font(style::INTER_SEMI))
                .on_press(Message::Nav(s))
                .padding([13, 16])
                .width(Length::Fill)
                .height(style::TARGET)
                .style(style::nav(active))
        };
        let (dot_colour, state) = match &self.status {
            Some(s) if s.live(data::unix_ms()) => (
                if s.state.starts_with("Recording") {
                    style::ACCENT
                } else if s.paused || !s.warnings.is_empty() {
                    style::WARNING
                } else {
                    style::MUTED
                },
                s.state.clone(),
            ),
            _ => (style::ERROR, "Not running".to_owned()),
        };
        container(
            column![
                row![
                    image(self.logo.clone()).width(36).height(36),
                    text("Retake").size(22).font(style::GROTESK_BOLD),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                Space::with_height(20),
                nav("Home", Screen::Home),
                nav("Library", Screen::Library),
                nav("Montage", Screen::Montage),
                nav("Settings", Screen::Settings),
                iced::widget::vertical_space(),
                row![
                    dot(dot_colour, 10.0),
                    text(state).size(13).color(style::MUTED),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text(
                    self.status
                        .as_ref()
                        .map(|s| format!("v{}", s.version))
                        .unwrap_or_default()
                )
                .size(12)
                .color(style::MUTED),
            ]
            .spacing(6)
            .padding(18),
        )
        .width(210)
        .height(Length::Fill)
        .style(style::sidebar)
        .into()
    }

    fn h1(&self, s: impl Into<String>) -> Element<'_, Message> {
        text(s.into()).size(28).font(style::GROTESK_BOLD).into()
    }

    fn home(&self) -> Element<'_, Message> {
        let mut col = column![].spacing(18);
        if !self.prefs.intro_seen {
            col = col.push(self.intro());
        }
        if !self.live() {
            col =
                col.push(
                    container(
                        row![
                            column![
                            text("Retake isn't running").size(18).font(style::GROTESK_BOLD),
                            text("Nothing is being recorded. Start it to record your games again.")
                                .color(style::MUTED),
                        ]
                            .spacing(4),
                            horizontal_space(),
                            button(text("Start Retake").font(style::INTER_SEMI))
                                .on_press(Message::StartService)
                                .padding([13, 18])
                                .style(style::primary),
                        ]
                        .align_y(Alignment::Center),
                    )
                    .padding(18)
                    .width(Length::Fill)
                    .style(style::banner(style::ERROR)),
                );
        }
        let s = self.status.clone().unwrap_or_default();
        let recording = s.state.starts_with("Recording");
        let headline = match (&s.game, recording) {
            _ if !self.live() => "Not recording".into(),
            (Some(g), true) => format!("Recording {g}"),
            (Some(g), false) => format!("{} · {g}", s.state),
            (None, _) if s.paused => "Paused".into(),
            (None, _) => "Ready: start a game and Retake records it".into(),
        };
        let pause = button(text(if s.paused { "Resume" } else { "Pause" }).font(style::INTER_SEMI))
            .on_press(Message::Send(Command::Pause { on: !s.paused }))
            .padding([13, 18])
            .style(style::secondary);
        let save = button(text("Save last 60 s").font(style::INTER_SEMI))
            .on_press(Message::Send(Command::SaveFull))
            .padding([13, 18])
            .style(style::primary);
        let state_panel = container(
            column![
                row![
                    dot(
                        if recording {
                            style::ACCENT
                        } else {
                            style::MUTED
                        },
                        18.0
                    ),
                    text(headline).size(30).font(style::GROTESK_BOLD),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                text(if s.capture.is_empty() {
                    format!(
                        "Press {} in game to save a clip",
                        if s.hotkey.is_empty() { "F9" } else { &s.hotkey }
                    )
                } else {
                    format!("{}   ·   press {} to save a clip", s.capture, s.hotkey)
                })
                .color(style::MUTED),
                row![
                    save,
                    pause,
                    button(text("Open clips folder"))
                        .on_press(Message::OpenFile(self.clips_dir().join("raw")))
                        .padding([13, 18])
                        .style(style::secondary),
                ]
                .spacing(10),
            ]
            .spacing(14),
        )
        .padding(22)
        .width(Length::Fill)
        .style(style::panel);
        col = col.push(state_panel);
        for w in &s.warnings {
            col = col.push(
                container(text(w.clone()).size(14))
                    .padding([13, 16])
                    .width(Length::Fill)
                    .style(style::banner(style::WARNING)),
            );
        }
        let tile = |label: &'static str, value: String| {
            container(
                column![
                    text(value).size(28).font(style::GROTESK_BOLD),
                    text(label).size(13).color(style::MUTED),
                ]
                .spacing(2),
            )
            .padding(16)
            .width(Length::Fill)
            .style(style::panel)
        };
        col = col.push(
            row![
                tile("Clips today", s.clips_today.to_string()),
                tile("This session", s.clips_session.to_string()),
                tile(
                    "Jobs waiting",
                    format!("{}{}", s.queued, if s.exporting { " ·" } else { "" })
                ),
                tile(
                    "Disk free",
                    s.disk_free_gb.map_or("—".into(), |g| format!("{g} GB"))
                ),
            ]
            .spacing(12),
        );
        // Today's clips.
        let now = data::unix_ms() / 1000;
        let today: Vec<&Clip> = self
            .clips
            .iter()
            .filter(|c| now - c.lib.saved < 86_400)
            .take(6)
            .collect();
        col = col.push(text("Today's clips").size(18).font(style::GROTESK_BOLD));
        if today.is_empty() {
            col = col.push(text("No clips yet today.").color(style::MUTED));
        } else {
            let mut r = row![].spacing(12);
            for c in today {
                r = r.push(self.thumb_button(c, 200.0, false));
            }
            col = col.push(scrollable(r).direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::default(),
            )));
        }
        scrollable(col.padding(iced::Padding {
            right: 16.0,
            ..Default::default()
        }))
        .into()
    }

    fn intro(&self) -> Element<'_, Message> {
        let hotkey = self
            .status
            .as_ref()
            .map(|s| s.hotkey.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "F9".into());
        let card = |title: String, body: String| {
            container(
                column![
                    text(title).size(18).font(style::GROTESK_BOLD),
                    text(body).size(14).color(style::MUTED),
                ]
                .spacing(6),
            )
            .padding(18)
            .width(Length::Fill)
            .style(style::panel)
        };
        let clips = self.clips_dir();
        column![
            row![
                text("Retake is already recording")
                    .size(20)
                    .font(style::GROTESK_BOLD),
                horizontal_space(),
                button(text("Got it"))
                    .on_press(Message::DismissIntro)
                    .padding([13, 16])
                    .style(style::secondary),
            ]
            .align_y(Alignment::Center),
            row![
                card(
                    format!("Press {hotkey}"),
                    format!("In any game, {hotkey} saves the last moments as a clip.")
                ),
                card(
                    "Your clips".into(),
                    format!("They go to {}.", clips.display())
                ),
                card(
                    "Kills clip themselves".into(),
                    "After each match Retake cuts your kills, 2Ks, 3Ks and aces automatically."
                        .into()
                ),
            ]
            .spacing(12),
        ]
        .spacing(12)
        .into()
    }

    /// A clip card: thumbnail (the hover strip animates), title, badges.
    fn thumb_button(&self, c: &Clip, width: f32, selected: bool) -> Element<'_, Message> {
        let h = width * 9.0 / 16.0;
        let hovered = self
            .hover
            .as_ref()
            .filter(|(p, f, _)| *p == c.lib.path && !f.is_empty());
        let pic: Element<'_, Message> = match (hovered, &c.thumb) {
            (Some((_, frames, i)), _) => image(frames[*i].clone())
                .width(width)
                .height(h)
                .content_fit(ContentFit::Cover)
                .into(),
            (None, Some(t)) => image(Handle::from_path(t))
                .width(width)
                .height(h)
                .content_fit(ContentFit::Cover)
                .into(),
            (None, None) => container(text("No thumbnail yet").size(12).color(style::MUTED))
                .width(width)
                .height(h)
                .center_x(width)
                .center_y(h)
                .style(|_| container::Style {
                    background: Some(iced::Background::Color(style::RAISED)),
                    ..Default::default()
                })
                .into(),
        };
        let now = data::unix_ms() / 1000;
        let kills = c.lib.kills.len();
        let mut badges = row![].spacing(6);
        if kills > 0 && c.play != "manual" {
            badges = badges.push(
                container(text(c.play.to_uppercase()).size(11).font(style::INTER_SEMI))
                    .padding([2, 6])
                    .style(style::badge(style::ACCENT_DIM)),
            );
        }
        if c.lib.starred {
            badges = badges.push(star(true, 14.0));
        }
        if !c.lib.game.is_empty() {
            badges = badges.push(
                container(text(data::game_name(&c.lib.game)).size(11))
                    .padding([2, 6])
                    .style(style::badge(style::RAISED)),
            );
        }
        let card = container(
            column![
                pic,
                column![
                    text(data::when(c.lib.saved, now))
                        .size(14)
                        .font(style::INTER_SEMI),
                    row![
                        text(if kills == 1 {
                            "1 kill".to_owned()
                        } else {
                            format!("{kills} kills")
                        })
                        .size(12)
                        .color(style::MUTED),
                        horizontal_space(),
                        badges,
                    ]
                    .align_y(Alignment::Center),
                ]
                .spacing(4)
                .padding([8, 10]),
            ]
            .width(width),
        )
        .style(style::card(selected));
        mouse_area(
            button(card)
                .on_press(Message::Open(c.lib.path.clone()))
                .padding(0)
                .style(style::ghost),
        )
        .on_enter(Message::HoverEnter(c.lib.path.clone()))
        .on_exit(Message::HoverExit)
        .into()
    }

    fn library(&self) -> Element<'_, Message> {
        let games = self.games_in_library();
        let filters = row![
            pick_list(games, Some(self.filter_game.clone()), Message::FilterGame).padding(13),
            pick_list(
                [Since::All, Since::Today, Since::Week, Since::Month],
                Some(self.filter_since),
                Message::FilterSince
            )
            .padding(13),
            checkbox("Starred", self.filter_starred)
                .on_toggle(Message::FilterStarred)
                .size(20),
            checkbox("Not used in a montage", self.filter_unused)
                .on_toggle(Message::FilterUnused)
                .size(20),
            button(text("Rescan"))
                .on_press(Message::Send(Command::Rescan))
                .padding([13, 16])
                .style(style::secondary),
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .wrap();
        let clips = self.visible_clips();
        let count = text(format!("{} clips", clips.len())).color(style::MUTED);
        let selected = self.selected;
        let grid = responsive(move |size| {
            let card_w = 290.0;
            let cols = ((size.width + 12.0) / (card_w + 12.0)).floor().max(1.0) as usize;
            let w = (size.width - 12.0 * (cols as f32 - 1.0)) / cols as f32;
            let mut col = column![].spacing(12);
            for (r, chunk) in self.visible_clips().chunks(cols).enumerate() {
                let mut line = row![].spacing(12);
                for (k, c) in chunk.iter().enumerate() {
                    line = line.push(self.thumb_button(c, w, r * cols + k == selected));
                }
                col = col.push(line);
            }
            if self.visible_clips().is_empty() {
                col = col.push(
                    text("No clips match. Save one with your clip hotkey in game.")
                        .color(style::MUTED),
                );
            }
            scrollable(col.padding(iced::Padding {
                right: 16.0,
                ..Default::default()
            }))
            .height(Length::Fill)
            .into()
        });
        column![self.h1("Library"), filters, count, grid]
            .spacing(14)
            .into()
    }

    fn player_view<'a>(
        &'a self,
        p: Option<&'a Player>,
        on_play: Message,
        max_h: f32,
    ) -> Element<'a, Message> {
        match p {
            Some(p) => {
                let frame: Element<'_, Message> = match &p.frame {
                    Some(f) => image(f.clone())
                        .height(max_h)
                        .content_fit(ContentFit::Contain)
                        .into(),
                    None => container(text("Loading…").color(style::MUTED))
                        .height(max_h)
                        .center_y(max_h)
                        .into(),
                };
                column![
                    container(frame).width(Length::Fill).center_x(Length::Fill),
                    row![
                        button(
                            text(if p.playing { "Pause" } else { "Play" }).font(style::INTER_SEMI)
                        )
                        .on_press(on_play)
                        .padding([13, 18])
                        .style(style::primary),
                        text(format!("{:.1} / {:.1} s", p.position, p.duration))
                            .color(style::MUTED),
                        horizontal_space(),
                        button(text("Open with sound"))
                            .on_press(Message::OpenFile(p.path.clone()))
                            .padding([13, 16])
                            .style(style::secondary),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                ]
                .spacing(10)
                .into()
            }
            None => container(text("Loading…").color(style::MUTED))
                .height(max_h)
                .center_y(max_h)
                .into(),
        }
    }

    fn clip_view(&self) -> Element<'_, Message> {
        let Some(v) = &self.view else {
            return text("No clip open").into();
        };
        let c = &v.clip;
        let title = row![
            button(text("Back to library"))
                .on_press(Message::Back)
                .padding([13, 16])
                .style(style::secondary),
            text(c.name()).size(22).font(style::GROTESK_BOLD),
            horizontal_space(),
            text(data::game_name(&c.lib.game)).color(style::MUTED),
        ]
        .spacing(14)
        .align_y(Alignment::Center);
        let duration = v.player.as_ref().map_or(c.lib.duration, |p| p.duration);
        let timeline: Element<'_, canvas::TimelineMsg> = iced::widget::canvas(canvas::Timeline {
            duration,
            position: v.player.as_ref().map_or(0.0, |p| p.position),
            kills: c.lib.kills.clone(),
            trim: v.trim,
        })
        .width(Length::Fill)
        .height(70)
        .into();
        let path = c.lib.path.clone();
        let export = |label: &'static str, preset: &'static str| {
            button(text(label))
                .on_press(Message::Send(Command::Export {
                    clip: path.clone(),
                    preset: preset.into(),
                }))
                .padding([13, 16])
                .style(style::secondary)
        };
        let options: Vec<Named2> = self
            .montages
            .iter()
            .map(|m| Named2(m.dir.to_string_lossy().into_owned(), m.name()))
            .collect();
        let trimmed = v.trim.0 > 0.05 || v.trim.1 < duration - 0.05;
        let actions = row![
            button(
                row![
                    star(c.lib.starred, 16.0),
                    text(if c.lib.starred { "Starred" } else { "Star" }).font(style::INTER_SEMI)
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            )
            .on_press(Message::Star)
            .padding([13, 16])
            .style(if c.lib.starred {
                style::primary
            } else {
                style::secondary
            }),
            export("Export share", "share"),
            export("Export vertical", "vertical"),
            export("Share with blur", "share+blur"),
            button(text("Regrade"))
                .on_press(Message::Send(Command::Regrade { clip: path.clone() }))
                .padding([13, 16])
                .style(style::secondary),
            button(text("Save trim"))
                .on_press_maybe(trimmed.then_some(Message::Trim))
                .padding([13, 16])
                .style(style::secondary),
        ]
        .spacing(8)
        .wrap();
        let more = row![
            pick_list(options, v.add_to.clone(), Message::AddTo)
                .placeholder("Montage…")
                .padding(13)
                .width(220),
            button(text("Add to montage"))
                .on_press_maybe(v.add_to.as_ref().map(|_| Message::AddToMontage))
                .padding([13, 16])
                .style(style::secondary),
            horizontal_space(),
            button(text("Show in folder"))
                .on_press(Message::Reveal(path.clone()))
                .padding([13, 16])
                .style(style::secondary),
            button(text("Delete"))
                .on_press(Message::Delete)
                .padding([13, 16])
                .style(style::danger),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);
        column![
            title,
            scrollable(
                column![
                    self.player_view(v.player.as_ref(), Message::PlayPause, 380.0),
                    timeline.map(Message::Timeline),
                    text(
                        "Drag the white handles to trim · Space plays · Left/Right seek 5 s · S stars"
                    )
                    .size(12)
                    .color(style::MUTED),
                    actions,
                    more,
                ]
                .spacing(12)
                .width(Length::Fill)
                .padding(iced::Padding {
                    right: 16.0,
                    ..Default::default()
                }),
            )
            .height(Length::Fill),
        ]
        .spacing(12)
        .into()
    }

    fn montage_view(&self) -> Element<'_, Message> {
        let games = self.games_in_library();
        let new = container(
            column![
                text("New montage").size(18).font(style::GROTESK_BOLD),
                row![
                    text_input("Song file (mp3, wav, ogg, flac)…", &self.new_song)
                        .on_input(Message::SongInput)
                        .padding(13)
                        .style(style::input),
                    button(text("Browse…"))
                        .on_press(Message::BrowseSong)
                        .padding([13, 16])
                        .style(style::secondary),
                ]
                .spacing(8),
                column![
                    toggler(self.new_youtube)
                        .label("YouTube 16:9")
                        .on_toggle(Message::NewYoutube),
                    toggler(self.new_starred)
                        .label("Starred clips only")
                        .on_toggle(Message::NewStarred),
                ]
                .spacing(8),
                row![
                    pick_list(games, Some(self.new_game.clone()), Message::NewGame)
                        .padding(13)
                        .width(Length::Fill),
                    text_input("Length (s)", &self.new_length)
                        .on_input(Message::NewLength)
                        .padding(13)
                        .width(110)
                        .style(style::input),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                button(
                    container(text("Make montage").font(style::INTER_SEMI)).center_x(Length::Fill)
                )
                .on_press(Message::CreateMontage)
                .padding([13, 18])
                .width(Length::Fill)
                .style(style::primary),
            ]
            .spacing(10),
        )
        .padding(16)
        .style(style::panel);
        let mut list = column![].spacing(6);
        for (i, m) in self.montages.iter().enumerate() {
            let sel = self.montage == Some(i);
            let rendered = m.dir.join("montage.mp4").is_file();
            list = list.push(
                button(
                    column![
                        text(m.name()).size(14).font(style::INTER_SEMI),
                        text(format!(
                            "{} · {:.0} s · {} clips{}",
                            if m.project.format == retake_core::montage::Format::Youtube {
                                "YouTube"
                            } else {
                                "TikTok"
                            },
                            m.project.length,
                            m.project.clips.iter().filter(|c| c.include).count(),
                            if rendered { " · rendered" } else { "" }
                        ))
                        .size(12)
                        .color(style::MUTED),
                    ]
                    .spacing(2),
                )
                .on_press(Message::SelectMontage(i))
                .padding([10, 12])
                .width(Length::Fill)
                .style(style::nav(sel)),
            );
        }
        if self.montages.is_empty() {
            list = list.push(text("No montages yet.").color(style::MUTED));
        }
        let left = column![
            new,
            text("Montages").size(18).font(style::GROTESK_BOLD),
            scrollable(list).height(Length::Fill)
        ]
        .spacing(12)
        .width(360);
        let right: Element<'_, Message> = match (self.current_montage(), &self.editor) {
            (Some(m), Some(e)) => self.editor_view(m, e),
            (Some(_), None) => text("Loading the montage…").color(style::MUTED).into(),
            (None, _) => container(
                text("Pick a montage on the left, or make a new one.").color(style::MUTED),
            )
            .center(Length::Fill)
            .into(),
        };
        column![
            self.h1("Montage"),
            row![left, right]
                .spacing(20)
                .height(Length::Fill)
                .width(Length::Fill)
        ]
        .spacing(14)
        .width(Length::Fill)
        .into()
    }

    fn editor_view<'a>(&'a self, m: &'a Montage, e: &'a Editor) -> Element<'a, Message> {
        let busy = self.status.as_ref().is_some_and(|s| s.montage_busy);
        let timeline: Element<'a, Message> = if e.analysed {
            let c: Element<'a, canvas::MontageMsg> = iced::widget::canvas(canvas::MontageView {
                length: m.project.length,
                peaks: e.peaks.clone(),
                beats: e.beats.clone(),
                drop: e.drop,
                blocks: e.blocks.clone(),
                selected: e.selected,
                playhead: self
                    .preview
                    .as_ref()
                    .filter(|p| p.playing)
                    .map(|p| p.position),
            })
            .width(Length::Fill)
            .height(190)
            .into();
            let length = m.project.length.max(0.1);
            let portion = |secs: f64| ((secs / length * 1000.0).round() as u16).max(1);
            // The drop's label, above the canvas at the drop's place.
            let drop_row: Element<'a, Message> = match e.drop {
                Some(d) => row![
                    Space::with_width(Length::FillPortion(portion(d))),
                    text("DROP")
                        .size(12)
                        .font(style::GROTESK_BOLD)
                        .color(style::ACCENT),
                    Space::with_width(Length::FillPortion(portion(length - d))),
                ]
                .padding([0, 12])
                .width(Length::Fill)
                .into(),
                None => Space::with_height(14).into(),
            };
            // Each block's role and clip, below the canvas under its block.
            let mut labels = row![].padding([0, 12]).width(Length::Fill);
            let mut blocks = e.blocks.clone();
            blocks.sort_by(|a, b| a.start.total_cmp(&b.start));
            let mut at = 0.0;
            for b in &blocks {
                if b.start > at + 0.01 {
                    labels = labels.push(Space::with_width(Length::FillPortion(portion(
                        b.start - at,
                    ))));
                }
                labels = labels.push(
                    container(
                        column![
                            text(b.role.clone())
                                .size(11)
                                .font(style::GROTESK_BOLD)
                                .wrapping(iced::widget::text::Wrapping::None),
                            text(b.label.clone())
                                .size(11)
                                .color(style::MUTED)
                                .wrapping(iced::widget::text::Wrapping::None),
                        ]
                        .spacing(1),
                    )
                    .width(Length::FillPortion(portion(b.end - b.start)))
                    .height(34)
                    .padding([0, 4])
                    .clip(true),
                );
                at = b.end;
            }
            if length > at + 0.01 {
                labels = labels.push(Space::with_width(Length::FillPortion(portion(length - at))));
            }
            column![drop_row, c.map(Message::MontageCanvas), labels]
                .width(Length::Fill)
                .spacing(2)
                .into()
        } else {
            container(text("The song hasn't been analysed yet: press Preview.").color(style::MUTED))
                .height(190)
                .center_y(190)
                .into()
        };
        let style_panel = row![
            toggler(m.project.format == retake_core::montage::Format::Youtube)
                .label("YouTube 16:9")
                .on_toggle(Message::StyleYoutube),
            text_input("Length s", &self.style_length)
                .on_input(Message::StyleLength)
                .padding(13)
                .width(90)
                .style(style::input),
            text_input("BPM (auto)", &self.style_bpm)
                .on_input(Message::StyleBpm)
                .padding(13)
                .width(100)
                .style(style::input),
            text_input("Song start s (auto)", &self.style_start)
                .on_input(Message::StyleStart)
                .padding(13)
                .width(150)
                .style(style::input),
            button(text("Apply"))
                .on_press(Message::ApplyStyle)
                .padding([13, 14])
                .style(style::secondary),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let timeline_fcpxml = m.dir.join("timeline.fcpxml");
        let actions = row![
            button(text("Preview").font(style::INTER_SEMI))
                .on_press_maybe((!busy).then_some(Message::RenderMontage(true)))
                .padding([13, 18])
                .style(style::secondary),
            button(text("Render").font(style::INTER_SEMI))
                .on_press_maybe((!busy).then_some(Message::RenderMontage(false)))
                .padding([13, 18])
                .style(style::primary),
            button(text("Resolve timeline"))
                .on_press_maybe(
                    timeline_fcpxml
                        .is_file()
                        .then(|| Message::Reveal(timeline_fcpxml.clone()))
                )
                .padding([13, 16])
                .style(style::secondary),
            button(text("Remove clip"))
                .on_press_maybe(e.selected.map(|_| Message::RemoveFromMontage))
                .padding([13, 16])
                .style(style::secondary),
            button(text("Play montage"))
                .on_press_maybe(
                    m.dir
                        .join("montage.mp4")
                        .is_file()
                        .then(|| Message::OpenFile(m.dir.join("montage.mp4")))
                )
                .padding([13, 16])
                .style(style::secondary),
            button(text("Open folder"))
                .on_press(Message::OpenFile(m.dir.clone()))
                .padding([13, 16])
                .style(style::secondary),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let progress: Element<'a, Message> =
            match self.status.as_ref().and_then(|s| s.montage_progress) {
                Some(p) if busy => row![
                    iced::widget::progress_bar(0.0..=1.0, p)
                        .height(10)
                        .style(|_| iced::widget::progress_bar::Style {
                            background: iced::Background::Color(style::RAISED),
                            bar: iced::Background::Color(style::ACCENT),
                            border: iced::Border {
                                radius: 5.0.into(),
                                ..Default::default()
                            },
                        }),
                    text(format!("{:.0} %", p * 100.0))
                        .size(13)
                        .color(style::MUTED),
                    button(text("Cancel"))
                        .on_press(Message::CancelRender)
                        .padding([13, 16])
                        .style(style::danger),
                ]
                .spacing(12)
                .align_y(Alignment::Center)
                .into(),
                _ => Space::with_height(0).into(),
            };
        let st = m.project.style;
        let slider_row = |label: &'static str,
                          value: f32,
                          range: std::ops::RangeInclusive<f32>,
                          step: f32,
                          shown: String,
                          on: fn(f32) -> Message| {
            row![
                text(label).size(13).width(130),
                iced::widget::slider(range, value, on)
                    .step(step)
                    .width(Length::Fill),
                text(shown).size(13).color(style::MUTED).width(56),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
        };
        use retake_core::montage::{Interpolation as I, Transitions as T};
        let effects = container(
            column![
                text("Style").size(15).font(style::GROTESK_BOLD),
                slider_row(
                    "Slow-mo on the drop",
                    st.slowmo as f32,
                    0.2..=0.6,
                    0.05,
                    format!("{:.2}x", st.slowmo),
                    Message::StyleSlowmo
                ),
                slider_row(
                    "Motion blur",
                    st.blur as f32,
                    0.0..=1.5,
                    0.1,
                    format!("{:.1}", st.blur),
                    Message::StyleBlur
                ),
                slider_row(
                    "Shake on kills",
                    st.shake as f32,
                    0.0..=1.0,
                    0.1,
                    format!("{:.1}", st.shake),
                    Message::StyleShake
                ),
                slider_row(
                    "Zoom punch",
                    st.punch as f32,
                    0.0..=0.15,
                    0.01,
                    format!("{:.0} %", st.punch * 100.0),
                    Message::StylePunch
                ),
                row![
                    toggler(st.music_in_video)
                        .label("Music in the video")
                        .on_toggle(Message::StyleMusic),
                    toggler(st.killfeed)
                        .label("Kill feed at the top (vertical)")
                        .on_toggle(Message::StyleKillfeed),
                ]
                .spacing(20)
                .wrap(),
                text(if st.music_in_video {
                    String::new()
                } else {
                    format!(
                        "Exported with game audio only. In TikTok, add the same sound starting at {}:{:02}.",
                        (e.song_start / 60.0) as u32,
                        (e.song_start % 60.0).round() as u32
                    )
                })
                .size(12)
                .color(style::ACCENT),
                row![
                    pick_list(
                        [
                            TransitionChoice(T::Cuts),
                            TransitionChoice(T::MostlyCuts),
                            TransitionChoice(T::Flashy)
                        ],
                        Some(TransitionChoice(st.transitions)),
                        Message::StyleTransitions
                    )
                    .padding(13),
                    pick_list(
                        [
                            InterpChoice(I::Auto),
                            InterpChoice(I::Always),
                            InterpChoice(I::Off)
                        ],
                        Some(InterpChoice(st.interpolation)),
                        Message::StyleInterpolation
                    )
                    .padding(13),
                ]
                .spacing(10)
                .wrap(),
            ]
            .spacing(8),
        )
        .padding(14)
        .width(Length::Fill)
        .style(style::panel);
        let status = if busy {
            text("Working on a montage… you can keep playing; Retake tells you when it's done.")
                .color(style::ACCENT)
        } else {
            text("Drag a clip onto another to swap places · drag its right edge to make it longer or shorter")
                .size(12)
                .color(style::MUTED)
        };
        let preview: Element<'a, Message> = if self.preview.is_some() {
            self.player_view(self.preview.as_ref(), Message::PreviewPlayPause, 300.0)
        } else {
            Space::with_height(0).into()
        };
        column![
            text(m.name()).size(20).font(style::GROTESK_BOLD),
            text(
                m.project
                    .song
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            )
            .color(style::MUTED),
            timeline,
            status,
            progress,
            scrollable(
                column![style_panel, effects, actions, preview]
                    .spacing(12)
                    .padding(iced::Padding {
                        right: 16.0,
                        ..Default::default()
                    }),
            )
            .height(Length::Fill),
        ]
        .spacing(12)
        .width(Length::Fill)
        .into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let st = &self.settings;
        let mut col = column![
            row![
                self.h1("Settings"),
                horizontal_space(),
                text_input("Search settings", &st.query)
                    .on_input(Message::SettingsQuery)
                    .padding(13)
                    .width(260)
                    .style(style::input),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(14);
        if let Some((ok, m)) = &st.message {
            col = col.push(
                container(text(m.clone()).size(14))
                    .padding([13, 16])
                    .width(Length::Fill)
                    .style(style::banner(if *ok { style::OK } else { style::WARNING })),
            );
        }
        let mut group = "";
        let mut panel = column![].spacing(10);
        let mut panels: Vec<Element<'_, Message>> = Vec::new();
        for (i, fl) in settings::FIELDS.iter().enumerate() {
            if !settings::matches(fl, &st.query) {
                continue;
            }
            if fl.group != group {
                if !group.is_empty() {
                    panels.push(
                        container(std::mem::replace(&mut panel, column![].spacing(10)))
                            .padding(16)
                            .width(Length::Fill)
                            .style(style::panel)
                            .into(),
                    );
                }
                group = fl.group;
                panel = panel.push(text(fl.group).size(17).font(style::GROTESK_BOLD));
            }
            let val = settings::value(&st.doc, &st.defaults, fl);
            let control: Element<'_, Message> = match fl.kind {
                settings::Kind::Toggle => toggler(val == "true")
                    .on_toggle(move |b| Message::SettingToggle(i, b))
                    .into(),
                settings::Kind::Choice(values) => {
                    let opts: Vec<String> = values
                        .iter()
                        .map(|v| v.trim_matches('"').to_owned())
                        .collect();
                    pick_list(opts, Some(val), move |v| Message::SettingValue(i, v))
                        .padding(13)
                        .into()
                }
                settings::Kind::Number | settings::Kind::Text => text_input("", &val)
                    .on_input(move |v| Message::SettingValue(i, v))
                    .padding(13)
                    .width(140)
                    .style(style::input)
                    .into(),
            };
            panel = panel.push(
                row![
                    column![
                        text(fl.label).size(14),
                        text(fl.help).size(12).color(style::MUTED),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    control,
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            );
        }
        if !group.is_empty() {
            panels.push(
                container(panel)
                    .padding(16)
                    .width(Length::Fill)
                    .style(style::panel)
                    .into(),
            );
        }
        for p in panels {
            col = col.push(p);
        }
        if settings::matches(
            &settings::Field {
                group: "Games",
                label: "Games profiles anti-cheat",
                help: "",
                section: "",
                key: "",
                kind: settings::Kind::Text,
            },
            &st.query,
        ) {
            col = col.push(self.games_panel());
        }
        col = col.push(
            container(
                column![
                    text("Window").size(17).font(style::GROTESK_BOLD),
                    toggler(self.prefs.reduce_motion)
                        .label("Reduce motion (no hover previews)")
                        .on_toggle(Message::ReduceMotion),
                    toggler(self.status.as_ref().is_some_and(|s| s.autostart))
                        .label("Start Retake with Windows")
                        .on_toggle(|on| Message::Send(Command::Autostart { on })),
                    row![
                        button(text("Copy diagnostics"))
                            .on_press(Message::Send(Command::CopyDiagnostics))
                            .padding([13, 16])
                            .style(style::secondary),
                        button(text("Open config file"))
                            .on_press(Message::OpenFile(self.args.config.clone()))
                            .padding([13, 16])
                            .style(style::secondary),
                        button(text("Open logs"))
                            .on_press(Message::OpenFile(platform::app_data().join("logs")))
                            .padding([13, 16])
                            .style(style::secondary),
                        button(text("Lite window"))
                            .on_press(Message::Send(Command::OpenLite))
                            .padding([13, 16])
                            .style(style::secondary),
                    ]
                    .spacing(8),
                ]
                .spacing(10),
            )
            .padding(16)
            .width(Length::Fill)
            .style(style::panel),
        );
        let bar = row![
            button(text("Save").font(style::INTER_SEMI))
                .on_press_maybe(st.dirty.then_some(Message::SaveSettings))
                .padding([13, 20])
                .style(style::primary),
            button(text("Revert"))
                .on_press_maybe(st.dirty.then_some(Message::RevertSettings))
                .padding([13, 16])
                .style(style::secondary),
            text(if st.dirty { "Unsaved changes" } else { "" }).color(style::MUTED),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        column![
            scrollable(col.padding(iced::Padding {
                right: 16.0,
                ..Default::default()
            }))
            .height(Length::Fill),
            bar
        ]
        .spacing(12)
        .into()
    }

    fn games_panel(&self) -> Element<'_, Message> {
        let rules = retake_core::config::load_str(&self.settings.doc.to_string())
            .map(|c| retake_core::games::Rules::from_config(&c))
            .unwrap_or_default();
        let mut col = column![
            text("Games").size(17).font(style::GROTESK_BOLD),
            text("Retake records these games with no setup. Automatic clips use each game's safest method.").size(12).color(style::MUTED),
        ]
        .spacing(8);
        let mut ids: Vec<String> = retake_core::games::BUILTIN
            .iter()
            .map(|p| p.id.to_owned())
            .collect();
        ids.extend(
            rules
                .overrides
                .keys()
                .filter(|k| !ids.contains(k))
                .cloned()
                .collect::<Vec<_>>(),
        );
        for id in ids {
            let Some(g) = rules.game_by_id(&id) else {
                continue;
            };
            let method = match g.method {
                retake_core::games::Method::SiegeReplay => "Match Replays",
                retake_core::games::Method::Gsi => "Game State Integration",
                retake_core::games::Method::LiveClient => "Live Client API",
                retake_core::games::Method::KillFeed => "Kill feed (unchecked layout)",
                retake_core::games::Method::Manual => "Clip hotkey only",
            };
            let manual = g.method == retake_core::games::Method::Manual;
            let (id_a, id_b) = (id.clone(), id.clone());
            let mut r = row![
                column![
                    text(g.name.clone()).size(14),
                    text(format!("{method} · {}", g.anticheat.name()))
                        .size(12)
                        .color(style::MUTED),
                ]
                .spacing(2)
                .width(Length::Fill),
            ]
            .spacing(16)
            .align_y(Alignment::Center);
            if !manual {
                r = r.push(
                    toggler(g.autoclip)
                        .label("Auto-clip")
                        .on_toggle(move |b| Message::GameSwitch(id_a.clone(), "autoclip", b)),
                );
            }
            r = r.push(
                toggler(g.strict)
                    .label("Strict")
                    .on_toggle(move |b| Message::GameSwitch(id_b.clone(), "strict", b)),
            );
            col = col.push(r);
        }
        col = col.push(
            row![
                text_input(
                    "Another game's exe, e.g. MyGame.exe",
                    &self.settings.add_exe
                )
                .on_input(Message::AddExe)
                .on_submit(Message::AddGame)
                .padding(13)
                .style(style::input),
                button(text("Add game"))
                    .on_press(Message::AddGame)
                    .padding([13, 16])
                    .style(style::secondary),
            ]
            .spacing(8),
        );
        container(col)
            .padding(16)
            .width(Length::Fill)
            .style(style::panel)
            .into()
    }
}

/// A coloured status dot.
fn dot<'a>(colour: iced::Color, size: f32) -> Element<'a, Message> {
    iced::widget::canvas(canvas::Dot(colour))
        .width(size)
        .height(size)
        .into()
}

/// A star (filled = starred), drawn once per size and kept.
fn star<'a>(filled: bool, size: f32) -> Element<'a, Message> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<(u32, bool), Handle>>> = Mutex::new(None);
    let px = size as u32 * 2;
    let h = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .entry((px, filled))
        .or_insert_with(|| Handle::from_rgba(px, px, style::star_rgba(px, filled)))
        .clone();
    image(h).width(size).height(size).into()
}

fn toggle_play(p: &mut Player) {
    p.playing = !p.playing;
    if p.playing {
        if p.position >= p.duration - 0.05 {
            p.position = 0.0;
        }
        p.from = p.position;
        p.generation += 1;
    }
}

fn on_player(p: &mut Player, e: player::Event) {
    match e {
        player::Event::Frame { at, image } => {
            if p.playing {
                p.position = at;
                p.frame = Some(image);
            }
        }
        player::Event::Ended => {
            p.playing = false;
            p.position = p.duration;
        }
    }
}
