//! Hardware style editor: a panel with coloured section bars, faders with
//! scales and stepped faders with waveform icons, plus graphs: a live wave
//! display per oscillator, a draggable filter response (cutoff / resonance),
//! draggable envelope shapes, an OLED that shows the control under the mouse,
//! and an oscilloscope.
//!
//! Everything is painted on a fixed `BW` x `BH` canvas through [`Cv`] and
//! scaled uniformly to the window, which the user resizes from the bottom
//! right corner.

pub mod theme;

use crate::params::{AdsrParams, FilterMode, LfoShape, OscParams, SynthParams, Waveform};
use crate::visuals::{ScopeOutput, VisualState, SCOPE_COLUMNS};
use crate::{filter, fm, osc, MAX_VOICES, N_PARTIALS};
use nih_plug::prelude::*;
use nih_plug_egui::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Id, Painter, Pos2, Rect,
    Response, Sense, Shape, Stroke, StrokeKind, TextureHandle, Ui, Vec2,
};
use nih_plug_egui::create_egui_editor;
use nih_plug_egui::resizable_window::ResizableWindow;
use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use theme::{Section, Theme, THEMES};

/// Canvas size in logical pixels. Everything is drawn on this canvas and scaled to the window.
const BW: f32 = 1400.0;
const BH: f32 = 850.0;
/// Default window size: the canvas at 110 %.
pub const WIDTH: u32 = (BW * 1.1) as u32;
pub const HEIGHT: u32 = (BH * 1.1) as u32;

/// Top of the section bar in each of the four rows. Labels, fader slots and
/// graphs hang off it at fixed offsets, see [`row`] and [`graph_rows`].
const BAR_Y: [f32; 4] = [46.0, 246.0, 446.0, 646.0];

/// Label baseline and fader slot (top, bottom) of a row.
fn row(i: usize) -> (f32, (f32, f32)) {
    let bar = BAR_Y[i];
    (bar + 46.0, (bar + 66.0, bar + 176.0))
}

/// Top and bottom of a graph that has a one-line caption above it.
fn graph_rows(i: usize) -> (f32, f32) {
    (BAR_Y[i] + 56.0, BAR_Y[i] + 186.0)
}

/// OLED and scope placement inside the MONITOR section.
const OLED: (f32, f32, f32, f32) = (798.0, 282.0, 292.0, 150.0);
const SCOPE: (f32, f32, f32, f32) = (1104.0, 282.0, 272.0, 150.0);

/// Range of the frequency axis in the filter display; also the cutoff range.
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
/// Floor of the output meter.
const METER_FLOOR_DB: f32 = -60.0;
/// Pitch the wave displays are computed at. Only matters for FM in fixed mode.
const DISPLAY_HZ: f32 = 220.0;

/// What the OLED draws next to the value of the focused control.
#[derive(Clone, Copy, PartialEq)]
enum FocusKind {
    Plain,
    /// Waveform of oscillator 0 or 1.
    Wave(usize),
    /// Resolved partial levels of oscillator 0 or 1.
    Partials(usize),
    /// LFO 1 or 2.
    Lfo(usize),
}

struct Focus {
    name: String,
    value: String,
    kind: FocusKind,
}

/// How long a value set by the GUI is shown before trusting the parameter again. nih-plug only
/// applies GUI changes asynchronously (CLAP queues them until the next process call or flush), so
/// reading the parameter right after setting it returns the old value.
const PENDING_TIMEOUT: f64 = 1.0;

struct Pending {
    value: f32,
    time: f64,
}

/// Shared parameter store used by the standalone preview and the tests, which have no plugin
/// wrapper that could apply parameter changes. Normalized values.
pub type PreviewStore = Arc<Mutex<HashMap<ParamPtr, f32>>>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlKind {
    Fader,
    Drawbar,
    Stepper,
    Button,
    /// Envelope breakpoint, registered under its horizontal parameter.
    Handle,
    /// Two-parameter graph, registered under its horizontal parameter.
    Graph,
}

/// Screen rectangle of an interactive control, recorded every frame (used by tests).
#[derive(Clone, Debug)]
pub struct ControlInfo {
    /// Parameter ID for parameter controls, a fixed key for buttons.
    pub name: String,
    pub kind: ControlKind,
    pub rect: Rect,
}

pub struct EditorState {
    visuals: Arc<VisualState>,
    scope: Arc<Mutex<ScopeOutput>>,
    /// Parameter IDs by pointer. Display names repeat between the two
    /// oscillators and the three envelopes, so controls are keyed by ID.
    /// Built from the parameters passed to [`root`], and rebuilt if they move.
    ids: HashMap<ParamPtr, String>,
    ids_for: usize,
    focus: Option<Focus>,
    /// Panel texture and the theme index it was made for.
    texture: Option<(u8, TextureHandle)>,
    /// Values sent to the host that may not have been applied to the parameters yet.
    pending: HashMap<ParamPtr, Pending>,
    preview_store: Option<PreviewStore>,
    pub controls: Vec<ControlInfo>,
}

impl EditorState {
    pub fn new(visuals: Arc<VisualState>, scope: Arc<Mutex<ScopeOutput>>) -> Self {
        Self {
            visuals,
            scope,
            ids: HashMap::new(),
            ids_for: 0,
            focus: None,
            texture: None,
            pending: HashMap::new(),
            preview_store: None,
            controls: Vec::new(),
        }
    }

    /// Read parameter values from `store` instead of the parameters themselves.
    pub fn with_preview_store(mut self, store: PreviewStore) -> Self {
        self.preview_store = Some(store);
        self
    }
}

pub fn create(
    params: Arc<SynthParams>,
    visuals: Arc<VisualState>,
    scope: Arc<Mutex<ScopeOutput>>,
) -> Option<Box<dyn Editor>> {
    let state = EditorState::new(visuals, scope);
    create_egui_editor(
        params.editor_state.clone(),
        state,
        |ctx, _| setup_style(ctx),
        move |ctx, setter, state| root(ctx, &params, setter, state),
    )
}

pub fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "panel".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/DejaVuSansCondensed-Bold.ttf"
        ))),
    );
    fonts.font_data.insert(
        "oled".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/DejaVuSansMono-Bold.ttf"
        ))),
    );
    fonts
        .families
        .insert(FontFamily::Name("panel".into()), vec!["panel".to_owned()]);
    fonts
        .families
        .insert(FontFamily::Name("oled".into()), vec!["oled".to_owned()]);
    ctx.set_fonts(fonts);
}

pub fn root(ctx: &egui::Context, params: &SynthParams, setter: &ParamSetter, state: &mut EditorState) {
    // The scope, meters and voice LEDs animate without any input.
    ctx.request_repaint();
    ResizableWindow::new("drift")
        .min_size(vec2(BW * 0.5, BH * 0.5))
        .show(ctx, params.editor_state.as_ref(), |ui| {
            draw(ui, params, setter, state)
        });
}

/// Maps canvas coordinates to screen coordinates.
#[derive(Clone, Copy)]
struct Cv {
    o: Pos2,
    s: f32,
}

impl Cv {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.o.x + x * self.s, self.o.y + y * self.s)
    }
    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(x, y), vec2(w * self.s, h * self.s))
    }
    fn rc(&self, cx: f32, cy: f32, w: f32, h: f32) -> Rect {
        self.r(cx - w * 0.5, cy - h * 0.5, w, h)
    }
    fn font(&self, size: f32) -> FontId {
        FontId::new(size * self.s, FontFamily::Name("panel".into()))
    }
    fn mono(&self, size: f32) -> FontId {
        FontId::new(size * self.s, FontFamily::Name("oled".into()))
    }
    fn stroke(&self, w: f32, c: Color32) -> Stroke {
        Stroke::new(w * self.s, c)
    }
    fn round(&self, r: f32) -> CornerRadius {
        CornerRadius::same((r * self.s) as u8)
    }
}

/// Scale legends next to a fader, top to bottom.
#[derive(Clone, Copy)]
enum Nums {
    None,
    Left(&'static str, &'static str, &'static str),
    Right(&'static str, &'static str, &'static str),
}

const N10L: Nums = Nums::Left("10", "5", "0");
const N10R: Nums = Nums::Right("10", "5", "0");
const BIPOLAR_L: Nums = Nums::Left("+", "0", "-");

/// Labels for the positions of a stepped fader, bottom position first.
#[derive(Clone, Copy)]
enum Steps {
    Text(&'static [&'static str]),
    Wave,
    Lfo,
}

struct G<'a, 'b> {
    ui: &'a Ui,
    pt: Painter,
    cv: Cv,
    set: &'a ParamSetter<'b>,
    st: &'a mut EditorState,
    th: &'static Theme,
    /// Accent of the section currently being drawn.
    accent: Color32,
}

fn draw(ui: &mut Ui, p: &SynthParams, set: &ParamSetter, st: &mut EditorState) {
    let full = ui.max_rect();
    let s = (full.width() / BW).min(full.height() / BH);
    let o = pos2(
        full.center().x - BW * s * 0.5,
        full.center().y - BH * s * 0.5,
    );
    let cv = Cv { o, s };

    let theme_index = p.theme.load(Relaxed);
    let th = theme::get(theme_index);
    if st.texture.as_ref().map(|(i, _)| *i) != Some(theme_index) {
        let tex = ui.ctx().load_texture(
            "panel-texture",
            panel_texture(th),
            egui::TextureOptions::LINEAR_REPEAT,
        );
        st.texture = Some((theme_index, tex));
    }
    if ui.style().visuals.panel_fill != th.outer {
        ui.ctx().style_mut(|s| s.visuals.panel_fill = th.outer);
    }
    let address = p as *const SynthParams as usize;
    if st.ids_for != address {
        st.ids = p.param_map().into_iter().map(|(id, ptr, _)| (ptr, id)).collect();
        st.ids_for = address;
    }
    st.focus = None;
    st.controls.clear();

    let pt = ui.painter().clone();
    let mut g = G { ui, pt, cv, set, st, th, accent: th.accents[0] };

    g.panel();
    g.header(p);

    g.bar(
        BAR_Y[0],
        &[
            (16.0, 470.0, "OSC 1", Section::Osc),
            (470.0, 924.0, "OSC 2", Section::Osc),
            (924.0, 1388.0, "FM", Section::Fm),
        ],
    );
    g.bar(
        BAR_Y[1],
        &[
            (16.0, 400.0, "ADDITIVE 1", Section::Osc),
            (400.0, 784.0, "ADDITIVE 2", Section::Osc),
            (784.0, 1388.0, "MONITOR", Section::Monitor),
        ],
    );
    g.bar(
        BAR_Y[2],
        &[
            (16.0, 546.0, "FILTER", Section::Filter),
            (546.0, 827.0, "AMP ENV", Section::Env),
            (827.0, 1108.0, "FILTER ENV", Section::Env),
            (1108.0, 1388.0, "FM ENV", Section::Env),
        ],
    );
    g.bar(
        BAR_Y[3],
        &[
            (16.0, 340.0, "LFO 1", Section::Lfo),
            (340.0, 580.0, "LFO 2", Section::Lfo),
            (580.0, 976.0, "VOICE", Section::Voice),
            (976.0, 1220.0, "CHORUS", Section::Output),
            (1220.0, 1388.0, "MASTER", Section::Output),
        ],
    );

    // ---------------------------------------------------------------- row 1
    g.accent = th.accents[Section::Osc as usize];
    g.osc(p, 0, 16.0);
    g.osc(p, 1, 470.0);

    g.accent = th.accents[Section::Fm as usize];
    let (l, sl) = row(0);
    let fm = &p.fm;
    g.stepper(&fm.mode, 976.0, l, sl, "MODE", Steps::Text(&["HARM", "FREE", "FIXED"]));
    let fm_faders: [(&FloatParam, &str, Nums); 8] = [
        (&fm.ratio, "RATIO", N10L),
        (&fm.ratio_fine, "RATIO\nFINE", Nums::None),
        (&fm.fixed_hz, "FIXED\nFREQ", Nums::None),
        (&fm.index1, "INDEX\nOSC 1", Nums::None),
        (&fm.index2, "INDEX\nOSC 2", Nums::None),
        (&fm.feedback, "FEED-\nBACK", Nums::None),
        (&fm.env_amount, "ENV\nAMOUNT", Nums::None),
        (&fm.keytrack, "KEY\nTRACK", N10R),
    ];
    for (i, (param, label, nums)) in fm_faders.into_iter().enumerate() {
        // Every FM setting changes both oscillator waves, so show the first.
        let kind = match i {
            4 => FocusKind::Wave(1),
            _ => FocusKind::Wave(0),
        };
        g.fader(param, 1028.0 + i as f32 * 46.0, l, sl, label, nums, kind);
    }

    // ---------------------------------------------------------------- row 2
    g.accent = th.accents[Section::Osc as usize];
    g.additive(&p.osc1, 0, 16.0);
    g.additive(&p.osc2, 1, 400.0);

    g.accent = th.accents[Section::Monitor as usize];
    g.scope();

    // ---------------------------------------------------------------- row 3
    g.accent = th.accents[Section::Filter as usize];
    g.filter_section(p);

    g.accent = th.accents[Section::Env as usize];
    g.env_section(&p.amp_env, 0, 546.0);
    g.env_section(&p.filter_env, 1, 827.0);
    g.env_section(&p.fm_env, 2, 1108.0);

    // ---------------------------------------------------------------- row 4
    g.accent = th.accents[Section::Lfo as usize];
    g.lfo_section(p, 0, 16.0);
    g.lfo_section(p, 1, 340.0);

    g.accent = th.accents[Section::Voice as usize];
    let (l, sl) = row(3);
    let v = &p.voice;
    g.stepper(&v.voice_count, 624.0, l, sl, "VOICES", Steps::Text(&["8", "16", "24", "32"]));
    g.stepper(
        &v.unison,
        678.0,
        l,
        sl,
        "UNISON",
        Steps::Text(&["1", "2", "3", "4", "5", "6", "7"]),
    );
    g.fader(&v.detune, 728.0, l, sl, "DETUNE", Nums::None, FocusKind::Plain);
    g.fader(&v.spread, 774.0, l, sl, "STEREO\nSPREAD", Nums::None, FocusKind::Plain);
    g.fader(&v.bend_range, 820.0, l, sl, "BEND\nRANGE", Nums::None, FocusKind::Plain);
    g.fader(&v.vibrato, 866.0, l, sl, "VIB-\nRATO", Nums::None, FocusKind::Plain);
    g.fader(&v.phase_random, 912.0, l, sl, "RAND\nPHASE", Nums::None, FocusKind::Plain);
    g.fader(&v.analog, 958.0, l, sl, "ANALOG", Nums::None, FocusKind::Plain);

    g.accent = th.accents[Section::Output as usize];
    let c = &p.chorus;
    g.stepper(&c.mode, 1026.0, l, sl, "MODE", Steps::Text(&["LUSH", "VINT"]));
    g.fader(&c.rate, 1072.0, l, sl, "RATE", Nums::None, FocusKind::Plain);
    g.fader(&c.depth, 1114.0, l, sl, "DEPTH", Nums::None, FocusKind::Plain);
    g.fader(&c.feedback, 1156.0, l, sl, "FEED-\nBACK", Nums::None, FocusKind::Plain);
    g.fader(&c.mix, 1198.0, l, sl, "MIX", Nums::None, FocusKind::Plain);

    let m = &p.master;
    g.fader(&m.gain, 1256.0, l, sl, "VOLUME", N10L, FocusKind::Plain);
    g.fader(&m.tune, 1300.0, l, sl, "TUNE", Nums::None, FocusKind::Plain);
    g.fader(&m.noise, 1342.0, l, sl, "NOISE", Nums::None, FocusKind::Plain);
    g.meter(1374.0, l, sl);

    g.accent = th.accents[Section::Monitor as usize];
    // Drawn last so it can show the focus set by any control this frame.
    g.oled(p);
}

/// Procedural powder coat: fine grain plus sparse lighter flecks.
fn panel_texture(th: &Theme) -> egui::ColorImage {
    let n = 256;
    let mut rng = crate::analog::Rng32::new(77);
    let mut px = Vec::with_capacity(n * n);
    for _ in 0..n * n {
        let grain = rng.bipolar() * th.grain;
        let fleck = if rng.unipolar() > 0.985 { th.fleck } else { 0.0 };
        let v = |c: u8| (c as f32 + grain + fleck).clamp(0.0, 255.0) as u8;
        px.push(Color32::from_rgb(v(th.panel.r()), v(th.panel.g()), v(th.panel.b())));
    }
    egui::ColorImage {
        size: [n, n],
        pixels: px,
    }
}

impl G<'_, '_> {
    fn text(&self, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32) {
        self.pt.text(pos, align, text, self.cv.font(size), color);
    }

    /// Multi-line label, bottom line sitting on `y`.
    fn label(&self, x: f32, y: f32, text: &str) {
        let lines: Vec<&str> = text.split('\n').collect();
        let n = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let yy = y - (n - 1 - i) as f32 * 11.0;
            self.text(self.cv.p(x, yy), Align2::CENTER_BOTTOM, line, 10.0, self.th.text);
        }
    }

    fn panel(&self) {
        let cv = self.cv;
        let rect = cv.r(0.0, 0.0, BW, BH);
        let tex = &self.st.texture.as_ref().unwrap().1;
        let uv = Rect::from_min_max(
            pos2(0.0, 0.0),
            pos2(rect.width() / (256.0 * cv.s), rect.height() / (256.0 * cv.s)),
        );
        self.pt.image(tex.id(), rect, uv, Color32::WHITE);
        self.pt.line_segment(
            [cv.p(2.0, 1.0), cv.p(BW - 2.0, 1.0)],
            cv.stroke(1.5, self.th.bevel),
        );
        self.pt.line_segment(
            [cv.p(2.0, BH - 1.0), cv.p(BW - 2.0, BH - 1.0)],
            cv.stroke(2.0, Color32::from_black_alpha(120)),
        );
    }

    fn header(&mut self, p: &SynthParams) {
        let cv = self.cv;
        self.text(
            cv.p(18.0, 30.0),
            Align2::LEFT_CENTER,
            "HYBRID ADDITIVE / SUBTRACTIVE POLYSYNTH",
            9.0,
            self.th.header,
        );
        self.voice_leds();
        self.theme_selector(p);
    }

    /// One cell per voice slot, lit to the voice's amplitude envelope level.
    /// The fastest way to see stuck notes or bad voice stealing.
    fn voice_leds(&self) {
        let cv = self.cv;
        let v = &self.st.visuals;
        let count = (v.voice_capacity.load(Relaxed) as usize).clamp(1, MAX_VOICES);
        let active = v.active_voices.load(Relaxed);
        self.text(cv.p(430.0, 22.0), Align2::RIGHT_CENTER, "VOICES", 8.5, self.th.header);
        let (x0, w, gap) = (438.0, 9.0, 2.0);
        for i in 0..MAX_VOICES {
            let rect = cv.r(x0 + i as f32 * (w + gap), 16.0, w, 12.0);
            if i >= count {
                self.pt.rect_stroke(
                    rect,
                    cv.round(1.5),
                    cv.stroke(0.8, self.th.led_off),
                    StrokeKind::Inside,
                );
                continue;
            }
            self.pt.rect_filled(rect, cv.round(1.5), self.th.led_off);
            let level = v.voice_env[i].load(Relaxed).clamp(0.0, 1.0);
            if level > 0.001 {
                let lit = self.th.led.gamma_multiply(0.35 + 0.65 * level);
                self.pt.rect_filled(rect, cv.round(1.5), lit);
            }
        }
        let end = x0 + MAX_VOICES as f32 * (w + gap) + 6.0;
        self.text(
            cv.p(end, 22.0),
            Align2::LEFT_CENTER,
            &format!("{active} / {count}"),
            8.5,
            self.th.header,
        );
    }

    /// Row of chips in the header that switches the colour scheme.
    fn theme_selector(&mut self, p: &SynthParams) {
        let cv = self.cv;
        let current = p.theme.load(Relaxed) as usize;
        let chip_w = |name: &str| 14.0 + name.len() as f32 * 5.6;
        let total: f32 = THEMES.iter().map(|t| chip_w(t.name) + 5.0).sum::<f32>() - 5.0;
        let mut x = 1388.0 - total;
        self.text(cv.p(x - 10.0, 22.0), Align2::RIGHT_CENTER, "THEME", 8.5, self.th.header);
        for (i, theme) in THEMES.iter().enumerate() {
            let w = chip_w(theme.name);
            let rect = cv.r(x, 15.0, w, 14.0);
            let key = format!("theme:{}", theme.name);
            let resp = self.ui.interact(rect, Id::new(&key), Sense::click());
            self.register(&key, ControlKind::Button, rect);
            let (fill, ink) = if i == current {
                (self.th.sublabel, self.th.bar_text)
            } else if resp.hovered() {
                (self.th.button_hover, self.th.text)
            } else {
                (self.th.button, self.th.text)
            };
            self.pt.rect_filled(rect, cv.round(3.0), fill);
            self.text(rect.center(), Align2::CENTER_CENTER, theme.name, 8.0, ink);
            if resp.clicked() {
                p.theme.store(i as u8, Relaxed);
            }
            if resp.hovered() {
                self.set_focus("COLOUR THEME", theme.name.to_owned(), FocusKind::Plain);
            }
            x += w + 5.0;
        }
    }

    fn bar(&self, y: f32, sections: &[(f32, f32, &str, Section)]) {
        let cv = self.cv;
        for (a, b, title, section) in sections {
            let color = self.th.bars[*section as usize];
            // Small gap between neighbouring sections
            let rect = cv.r(a + 1.5, y, b - a - 3.0, 22.0);
            self.pt.rect_filled(rect, cv.round(3.0), color);
            self.text(
                cv.p((a + b) * 0.5, y + 11.5),
                Align2::CENTER_CENTER,
                title,
                15.0,
                self.th.bar_text,
            );
        }
    }

    // ------------------------------------------------------------ sections

    /// Wave display on the left, then the waveform stepper and faders.
    fn osc(&mut self, p: &SynthParams, index: usize, x0: f32) {
        let (l, sl) = row(0);
        let o = Self::osc_params(p, index);
        let kind = FocusKind::Wave(index);
        let rect = self.cv.r(x0 + 12.0, BAR_Y[0] + 34.0, 140.0, 152.0);
        self.wave_display(p, index, rect);
        self.stepper(&o.wave, x0 + 198.0, l, sl, "WAVE", Steps::Wave);
        self.fader(&o.coarse, x0 + 250.0, l, sl, "COARSE", Nums::None, kind);
        self.fader(&o.fine, x0 + 294.0, l, sl, "FINE", Nums::None, kind);
        self.fader(&o.pulse_width, x0 + 338.0, l, sl, "PULSE\nWIDTH", Nums::None, kind);
        self.fader(&o.level, x0 + 382.0, l, sl, "LEVEL", Nums::None, kind);
        self.fader(&o.blend, x0 + 426.0, l, sl, "ADD\nBLEND", N10R, kind);
    }

    /// Eight drawbars plus the three macros that shape them.
    fn additive(&mut self, o: &OscParams, index: usize, x0: f32) {
        let (l, sl) = row(1);
        let kind = FocusKind::Partials(index);
        let resolved = self.resolved_partials(o);
        for (k, partial) in o.partials.iter().enumerate() {
            let x = x0 + 30.0 + k as f32 * 28.0;
            self.drawbar(&partial.amp, x, l, sl, k, resolved[k], kind);
        }
        self.fader(&o.tilt, x0 + 278.0, l, sl, "TILT", BIPOLAR_L, kind);
        self.fader(&o.partial_decay, x0 + 322.0, l, sl, "DECAY", Nums::None, kind);
        self.fader(&o.odd_even, x0 + 366.0, l, sl, "ODD /\nEVEN", Nums::None, kind);
    }

    /// Mode stepper, the cutoff / resonance graph and the remaining faders.
    fn filter_section(&mut self, p: &SynthParams) {
        let (l, sl) = row(2);
        let f = &p.filter;
        self.stepper(
            &f.mode,
            72.0,
            l,
            sl,
            "MODE",
            Steps::Text(&["LP24", "LP12", "HP24", "HP12", "BP24", "BP12", "NOTCH"]),
        );
        let (top, bottom) = graph_rows(2);
        let rect = self.cv.r(100.0, top, 220.0, bottom - top);
        self.filter_graph(p, rect);
        let cutoff = format!("CUTOFF  {}", self.value_text(&f.cutoff));
        let reso = format!("RESO  {}", self.value_text(&f.resonance));
        self.text(self.cv.p(100.0, l), Align2::LEFT_BOTTOM, &cutoff, 10.0, self.th.text);
        self.text(self.cv.p(320.0, l), Align2::RIGHT_BOTTOM, &reso, 10.0, self.th.text);

        let faders: [(&FloatParam, &str, Nums); 5] = [
            (&f.drive, "DRIVE", Nums::None),
            (&f.keytrack, "KEY\nTRACK", Nums::None),
            (&f.env_amount, "ENV\nAMOUNT", Nums::None),
            (&f.vel_amount, "VELO-\nCITY", Nums::None),
            (&f.reso_comp, "RESO\nCOMP", N10R),
        ];
        for (i, (param, label, nums)) in faders.into_iter().enumerate() {
            self.fader(param, 352.0 + i as f32 * 44.0, l, sl, label, nums, FocusKind::Plain);
        }
    }

    /// Envelope graph with drag handles, plus the curve fader.
    fn env_section(&mut self, env: &AdsrParams, which: usize, x0: f32) {
        let (l, sl) = row(2);
        let (top, bottom) = graph_rows(2);
        let rect = self.cv.r(x0 + 12.0, top, 200.0, bottom - top);
        for (t, name) in [(0.1, "ATTACK"), (0.36, "DECAY"), (0.62, "SUSTAIN"), (0.88, "RELEASE")] {
            self.text(
                self.cv.p(x0 + 12.0 + 200.0 * t, l),
                Align2::CENTER_BOTTOM,
                name,
                9.0,
                self.th.legend,
            );
        }
        self.env_graph(env, which, rect);
        self.fader(&env.curve, x0 + 248.0, l, sl, "CURVE", Nums::None, FocusKind::Plain);
    }

    /// One LFO. Delay and fade-in only exist per voice, so LFO 2 (global) has none.
    fn lfo_section(&mut self, p: &SynthParams, which: usize, x0: f32) {
        let (l, sl) = row(3);
        let lfo = if which == 0 { &p.lfo1 } else { &p.lfo2 };
        let kind = FocusKind::Lfo(which);
        let live = match which {
            0 => self.st.visuals.lfo1_value.load(Relaxed),
            _ => self.st.visuals.lfo2_value.load(Relaxed),
        };
        self.led_level(self.cv.p(x0 + 12.0, l - 5.0), 0.5 + 0.5 * live.clamp(-1.0, 1.0));
        self.stepper(&lfo.shape, x0 + 52.0, l, sl, "SHAPE", Steps::Lfo);
        self.fader(&lfo.rate, x0 + 100.0, l, sl, "RATE", Nums::None, kind);
        self.fader(&lfo.depth, x0 + 144.0, l, sl, "DEPTH", Nums::None, kind);
        self.stepper(
            &lfo.dest,
            x0 + 222.0,
            l,
            sl,
            "DEST",
            Steps::Text(&["OFF", "PITCH", "CUTOFF", "AMP", "PW", "FM", "BLEND"]),
        );
        if which == 0 {
            self.fader(&lfo.delay, x0 + 264.0, l, sl, "DELAY", Nums::None, kind);
            self.fader(&lfo.fade, x0 + 306.0, l, sl, "FADE\nIN", Nums::None, kind);
        }
    }

    // ------------------------------------------------------- parameter I/O

    fn register(&mut self, name: &str, kind: ControlKind, rect: Rect) {
        self.st.controls.push(ControlInfo {
            name: name.to_owned(),
            kind,
            rect,
        });
    }

    fn register_param<P: Param>(&mut self, param: &P, kind: ControlKind, rect: Rect) {
        let name = self
            .st
            .ids
            .get(&param.as_ptr())
            .cloned()
            .unwrap_or_else(|| param.name().to_owned());
        self.register(&name, kind, rect);
    }

    fn now(&self) -> f64 {
        self.ui.input(|i| i.time)
    }

    /// The parameter's actual normalized value (from the preview store in the standalone preview).
    fn actual<P: Param>(&self, param: &P) -> f32 {
        if let Some(store) = &self.st.preview_store {
            if let Some(v) = store.lock().unwrap().get(&param.as_ptr()) {
                return *v;
            }
        }
        param.unmodulated_normalized_value()
    }

    /// The value to display: a value the GUI just sent wins until the host has applied it.
    fn shown<P: Param>(&mut self, param: &P) -> f32 {
        let actual = self.actual(param);
        let now = self.now();
        let key = param.as_ptr();
        match self.st.pending.get(&key) {
            Some(p) if (p.value - actual).abs() > 1e-5 && now - p.time < PENDING_TIMEOUT => p.value,
            Some(_) => {
                self.st.pending.remove(&key);
                actual
            }
            None => actual,
        }
    }

    /// Plain value as currently displayed.
    fn plain<P: Param>(&mut self, param: &P) -> P::Plain {
        let v = self.shown(param);
        param.preview_plain(v)
    }

    /// Send a normalized value (snapped to the parameter's steps) and remember it as pending.
    /// Must be called between `begin_set_parameter` and `end_set_parameter`.
    fn send<P: Param>(&mut self, param: &P, normalized: f32) {
        let v = param.preview_normalized(param.preview_plain(normalized.clamp(0.0, 1.0)));
        if (v - self.shown(param)).abs() < 1e-6 {
            return;
        }
        self.set.set_parameter_normalized(param, v);
        let time = self.now();
        self.st.pending.insert(param.as_ptr(), Pending { value: v, time });
    }

    /// A complete single-step change (begin, set, end).
    fn change<P: Param>(&mut self, param: &P, normalized: f32) {
        self.set.begin_set_parameter(param);
        self.send(param, normalized);
        self.set.end_set_parameter(param);
    }

    fn value_text<P: Param>(&mut self, param: &P) -> String {
        let v = self.shown(param);
        param.normalized_value_to_string(v, true)
    }

    fn set_focus(&mut self, name: &str, value: String, kind: FocusKind) {
        self.st.focus = Some(Focus {
            name: name.to_uppercase(),
            value,
            kind,
        });
    }

    /// Mouse handling shared by faders and drawbars: vertical drag (Shift for
    /// fine), wheel, double-click to reset. The drag position is tracked
    /// locally (start value plus accumulated movement) because the parameter
    /// itself only updates asynchronously.
    fn continuous_input<P: Param>(&mut self, param: &P, rect: Rect, travel: f32, kind: FocusKind) -> Response {
        let id = Id::new(("fader", param.as_ptr()));
        let resp = self.ui.interact(rect, id, Sense::click_and_drag());

        if resp.drag_started() {
            let start = self.shown(param);
            self.ui.data_mut(|d| d.insert_temp(id, start));
            self.set.begin_set_parameter(param);
        }
        if resp.dragged() {
            let fine = self.ui.input(|i| i.modifiers.shift);
            let delta = -resp.drag_delta().y / (travel * self.cv.s) * if fine { 0.15 } else { 1.0 };
            let start = self.shown(param);
            let pos: f32 = self.ui.data(|d| d.get_temp(id)).unwrap_or(start);
            let pos = (pos + delta).clamp(0.0, 1.0);
            self.ui.data_mut(|d| d.insert_temp(id, pos));
            self.send(param, pos);
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(param);
        }
        if resp.double_clicked() {
            self.change(param, param.default_normalized_value());
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                // Integer parameters need at least one whole step per notch.
                let step = param
                    .step_count()
                    .map(|n| 1.0 / n as f32)
                    .unwrap_or(0.0)
                    .max(0.0015 * scroll.abs());
                let v = self.shown(param) + step * scroll.signum();
                self.change(param, v);
            }
        }
        if resp.hovered() || resp.dragged() {
            let text = self.value_text(param);
            self.set_focus(param.name(), text, kind);
        }
        resp
    }

    // ----------------------------------------------------------- controls

    fn scale_ticks(&self, x: f32, slot: (f32, f32), nums: Nums) {
        let cv = self.cv;
        let (y0, y1) = slot;
        for i in 0..11 {
            let y = y1 - i as f32 / 10.0 * (y1 - y0);
            let major = i % 5 == 0;
            let w = if major { 15.0 } else { 12.0 };
            self.pt.line_segment(
                [cv.p(x - w, y), cv.p(x + w, y)],
                cv.stroke(if major { 1.2 } else { 0.9 }, self.th.scale),
            );
        }
        let (side, legends) = match nums {
            Nums::None => return,
            Nums::Left(a, b, c) => (-1.0, [a, b, c]),
            Nums::Right(a, b, c) => (1.0, [a, b, c]),
        };
        let align = if side < 0.0 { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
        for (k, text) in legends.iter().enumerate() {
            let y = y0 + k as f32 * 0.5 * (y1 - y0);
            self.text(cv.p(x + side * 18.0, y), align, text, 9.0, self.th.legend);
        }
    }

    fn slot(&self, x: f32, slot: (f32, f32)) {
        let cv = self.cv;
        self.pt.rect_filled(
            cv.r(x - 2.5, slot.0 - 8.0, 5.0, slot.1 - slot.0 + 16.0),
            cv.round(2.0),
            self.th.slot,
        );
    }

    fn cap(&self, x: f32, y: f32, w: f32, active: bool) {
        let cv = self.cv;
        let body = cv.rc(x, y, w, 30.0);
        let round = cv.round(2.0);
        self.pt.rect_filled(
            body.translate(vec2(1.5 * cv.s, 3.0 * cv.s)),
            round,
            Color32::from_black_alpha(140),
        );
        self.pt.rect_filled(body, round, self.th.cap);
        // Sloped faces above and below the index line
        self.pt
            .rect_filled(cv.rc(x, y - 8.0, w - 2.0, 10.0), cv.round(1.0), self.th.cap_top);
        self.pt
            .rect_filled(cv.rc(x, y + 8.0, w - 2.0, 10.0), cv.round(1.0), self.th.cap_bottom);
        self.pt.line_segment(
            [cv.p(x - w * 0.45, y), cv.p(x + w * 0.45, y)],
            cv.stroke(2.2, if active { self.th.drag_line } else { self.accent }),
        );
    }

    /// Continuous fader for any parameter.
    #[allow(clippy::too_many_arguments)]
    fn fader<P: Param>(
        &mut self,
        param: &P,
        x: f32,
        label_y: f32,
        slot: (f32, f32),
        label: &str,
        nums: Nums,
        kind: FocusKind,
    ) {
        let (y0, y1) = slot;
        let rect = self.cv.r(x - 14.0, y0 - 16.0, 28.0, y1 - y0 + 32.0);
        self.register_param(param, ControlKind::Fader, rect);
        let resp = self.continuous_input(param, rect, y1 - y0, kind);

        self.label(x, label_y, label);
        self.scale_ticks(x, slot, nums);
        self.slot(x, slot);
        let v = self.shown(param);
        self.cap(x, y1 - v * (y1 - y0), 20.0, resp.dragged());
    }

    /// Narrow fader for one additive partial. Behind the slot, a column shows
    /// the partial's level after TILT, DECAY and ODD/EVEN, which would
    /// otherwise be invisible in their effect.
    #[allow(clippy::too_many_arguments)]
    fn drawbar(
        &mut self,
        param: &FloatParam,
        x: f32,
        label_y: f32,
        slot: (f32, f32),
        index: usize,
        resolved: f32,
        kind: FocusKind,
    ) {
        let cv = self.cv;
        let (y0, y1) = slot;
        let rect = cv.r(x - 12.0, y0 - 16.0, 24.0, y1 - y0 + 32.0);
        self.register_param(param, ControlKind::Drawbar, rect);
        let resp = self.continuous_input(param, rect, y1 - y0, kind);

        self.label(x, label_y, &(index + 1).to_string());
        self.pt.rect_filled(
            cv.r(x - 6.0, y0, 12.0, y1 - y0),
            cv.round(1.0),
            Color32::from_black_alpha(60),
        );
        let h = resolved.clamp(0.0, 1.0) * (y1 - y0);
        if h > 0.2 {
            self.pt.rect_filled(
                cv.r(x - 6.0, y1 - h, 12.0, h),
                cv.round(1.0),
                self.accent.gamma_multiply(0.45),
            );
        }
        self.slot(x, slot);
        let v = self.shown(param);
        self.cap(x, y1 - v * (y1 - y0), 16.0, resp.dragged());
    }

    /// Fader with detents; positions are labelled with text or icons on the
    /// left, lowest parameter value at the bottom.
    #[allow(clippy::too_many_arguments)]
    fn stepper<P: Param>(
        &mut self,
        param: &P,
        x: f32,
        label_y: f32,
        slot: (f32, f32),
        label: &str,
        steps: Steps,
    ) {
        let cv = self.cv;
        let count = param.step_count().unwrap_or(1) + 1;
        let (y0, y1) = slot;
        let last = (count - 1) as f32;
        let pos_y = |i: usize| y1 - i as f32 / last * (y1 - y0);
        let rect = cv.r(x - 40.0, y0 - 16.0, 54.0, y1 - y0 + 32.0);
        let id = Id::new(("step", param.as_ptr()));
        let resp = self.ui.interact(rect, id, Sense::click_and_drag());
        self.register_param(param, ControlKind::Stepper, rect);

        let current = (self.shown(param) * last).round() as usize;
        let mut target = None;

        if resp.drag_started() {
            self.ui.data_mut(|d| d.insert_temp(id, current as f32));
            self.set.begin_set_parameter(param);
        }
        if resp.dragged() {
            let step_px = (y1 - y0) / last * cv.s;
            let mut acc: f32 = self.ui.data(|d| d.get_temp(id)).unwrap_or(current as f32);
            acc = (acc - resp.drag_delta().y / step_px).clamp(0.0, last);
            self.ui.data_mut(|d| d.insert_temp(id, acc));
            self.send(param, acc.round() / last);
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(param);
        }
        if resp.clicked() {
            if let Some(ptr) = resp.interact_pointer_pos() {
                let local_y = (ptr.y - cv.o.y) / cv.s;
                target = (0..count).min_by(|a, b| {
                    (pos_y(*a) - local_y)
                        .abs()
                        .total_cmp(&(pos_y(*b) - local_y).abs())
                });
            }
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll > 0.0 && current + 1 < count {
                target = Some(current + 1);
            } else if scroll < 0.0 && current > 0 {
                target = Some(current - 1);
            }
        }
        if let Some(i) = target {
            self.change(param, i as f32 / last);
        }
        if resp.hovered() || resp.dragged() {
            let text = self.value_text(param);
            self.set_focus(param.name(), text, FocusKind::Plain);
        }

        let shown = (self.shown(param) * last).round() as usize;
        self.label(x, label_y, label);
        for i in 0..count {
            let y = pos_y(i);
            self.pt.line_segment(
                [cv.p(x - 9.0, y), cv.p(x + 12.0, y)],
                cv.stroke(1.2, self.th.scale),
            );
            self.step_label(steps, i, x - 14.0, y, i == shown);
        }
        self.slot(x, slot);
        self.cap(x, pos_y(shown), 20.0, resp.dragged());
    }

    fn step_label(&self, steps: Steps, i: usize, right: f32, y: f32, selected: bool) {
        let cv = self.cv;
        let color = if selected { self.accent } else { self.th.step_off };
        let icon = Rect::from_min_max(cv.p(right - 20.0, y - 4.5), cv.p(right - 1.0, y + 4.5));
        match steps {
            Steps::Text(t) => {
                self.text(cv.p(right, y), Align2::RIGHT_CENTER, t[i], 9.0, color);
            }
            Steps::Wave => {
                let wave = [Waveform::Sine, Waveform::Triangle, Waveform::Saw, Waveform::Pulse][i];
                let pts = sample_icon(icon, 2.0, |ph| naive_wave(wave, ph));
                self.pt.add(Shape::line(pts, Stroke::new(1.1 * cv.s, color)));
            }
            Steps::Lfo => {
                let pts = sample_icon(icon, 2.0, |ph| lfo_shape(i, ph));
                self.pt.add(Shape::line(pts, Stroke::new(1.1 * cv.s, color)));
            }
        }
    }

    /// LED lit to `level` in 0..1.
    fn led_level(&self, c: Pos2, level: f32) {
        let s = self.cv.s;
        if level > 0.0 {
            self.pt
                .circle_filled(c, 6.0 * s, self.th.led.gamma_multiply(0.25 * level));
        }
        self.pt.circle_filled(c, 3.2 * s, self.th.led_off);
        if level > 0.0 {
            self.pt
                .circle_filled(c, 3.2 * s, self.th.led.gamma_multiply(level));
        }
        self.pt
            .circle_stroke(c, 3.2 * s, Stroke::new(0.8 * s, self.th.slot));
    }

    /// Stereo peak meter, two vertical bars with a tick at -6 dBFS.
    fn meter(&self, x: f32, label_y: f32, slot: (f32, f32)) {
        let cv = self.cv;
        let (y0, y1) = slot;
        let position = |gain: f32| {
            let db = 20.0 * gain.max(1.0e-6).log10();
            ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
        };
        self.label(x, label_y, "OUT");
        let v = &self.st.visuals;
        for (k, gain) in [v.peak_l.load(Relaxed), v.peak_r.load(Relaxed)].into_iter().enumerate() {
            let bx = x - 6.0 + k as f32 * 7.0;
            self.pt
                .rect_filled(cv.r(bx, y0, 5.0, y1 - y0), cv.round(1.0), self.th.slot);
            let h = position(gain) * (y1 - y0);
            if h > 0.2 {
                let color = if gain > 1.0 { self.th.clip } else { self.accent };
                self.pt
                    .rect_filled(cv.r(bx, y1 - h, 5.0, h), cv.round(1.0), color);
            }
        }
        let tick = y1 - position(0.5) * (y1 - y0);
        self.pt.line_segment(
            [cv.p(x - 9.0, tick), cv.p(x + 9.0, tick)],
            cv.stroke(1.0, self.th.scale),
        );
    }

    // ------------------------------------------------------------ displays

    fn screen(&self, r: (f32, f32, f32, f32)) -> Rect {
        let cv = self.cv;
        let rect = cv.r(r.0, r.1, r.2, r.3);
        self.pt
            .rect_filled(rect.expand(3.0 * cv.s), cv.round(3.0), self.th.oled_frame);
        self.pt.rect_filled(rect, CornerRadius::ZERO, self.th.oled_bg);
        rect
    }

    /// Recessed background behind the panel graphs.
    fn graph_bg(&self, rect: Rect) {
        self.pt
            .rect_filled(rect, self.cv.round(3.0), self.th.slot.gamma_multiply(0.55));
    }

    /// Fill between a polyline and a horizontal baseline. The area is usually
    /// not convex, so it is built as a strip of quads.
    fn fill_under(&self, pts: &[Pos2], base: f32, color: Color32) {
        let mut mesh = egui::Mesh::default();
        for w in pts.windows(2) {
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(w[0], color);
            mesh.colored_vertex(w[1], color);
            mesh.colored_vertex(pos2(w[1].x, base), color);
            mesh.colored_vertex(pos2(w[0].x, base), color);
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
        self.pt.add(Shape::mesh(mesh));
    }

    /// Square drag handle, highlighted while hovered or dragged.
    fn handle_square(&self, c: Pos2, active: bool) {
        let s = self.cv.s;
        let r = Rect::from_center_size(c, vec2(9.0, 9.0) * s);
        let fill = if active { self.th.drag_line } else { self.th.text };
        self.pt.rect_filled(r, CornerRadius::same((1.5 * s) as u8), fill);
        self.pt.rect_stroke(r, CornerRadius::same((1.5 * s) as u8), Stroke::new(1.2 * s, self.th.slot), StrokeKind::Outside);
    }

    /// Min/max envelope of the output.
    fn scope(&self) {
        let rect = self.screen(SCOPE);
        let s = self.cv.s;
        let ink = self.th.oled_ink;
        let mid = rect.center().y;
        self.pt.line_segment(
            [pos2(rect.left(), mid), pos2(rect.right(), mid)],
            Stroke::new(0.6 * s, self.th.oled_dim),
        );
        self.pt.text(
            rect.left_top() + vec2(6.0, 4.0) * s,
            Align2::LEFT_TOP,
            "OUTPUT",
            self.cv.mono(8.5),
            self.th.oled_dim,
        );
        let Ok(mut output) = self.st.scope.lock() else {
            return;
        };
        let frame = output.read();
        let half = rect.height() * 0.5 - 4.0 * s;
        let x_of = |i: usize| rect.left() + i as f32 * rect.width() / (SCOPE_COLUMNS - 1) as f32;
        let y_of = |v: f32| mid - v.clamp(-1.0, 1.0) * half;
        let band: Vec<Pos2> = (0..SCOPE_COLUMNS).map(|i| pos2(x_of(i), y_of(frame.max[i]))).collect();
        let mut mesh = egui::Mesh::default();
        let fill = ink.gamma_multiply(0.22);
        for i in 0..SCOPE_COLUMNS - 1 {
            let k = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(x_of(i), y_of(frame.max[i])), fill);
            mesh.colored_vertex(pos2(x_of(i + 1), y_of(frame.max[i + 1])), fill);
            mesh.colored_vertex(pos2(x_of(i + 1), y_of(frame.min[i + 1])), fill);
            mesh.colored_vertex(pos2(x_of(i), y_of(frame.min[i])), fill);
            mesh.add_triangle(k, k + 1, k + 2);
            mesh.add_triangle(k, k + 2, k + 3);
        }
        self.pt.add(Shape::mesh(mesh));
        self.pt.add(Shape::line(band, Stroke::new(1.0 * s, ink)));
    }

    /// Value display: the control under the mouse with its value and a
    /// matching graph, or the voice count and the mixed oscillator wave.
    fn oled(&mut self, p: &SynthParams) {
        let cv = self.cv;
        let th = self.th;
        self.screen(OLED);
        let (w, h) = (OLED.2, OLED.3);
        let tl = |x: f32, y: f32| cv.p(OLED.0 + 8.0 + x, OLED.1 + 6.0 + y);
        let graph = Rect::from_min_max(tl(146.0, 8.0), tl(w - 18.0, h - 44.0));
        let txt = |pt: &Painter, pos: Pos2, text: &str, size: f32, color: Color32| {
            pt.text(pos, Align2::LEFT_TOP, text, cv.mono(size), color);
        };

        match self.st.focus.take() {
            Some(f) => {
                txt(&self.pt, tl(0.0, 0.0), &f.name, 8.5, th.oled_dim);
                // Two-parameter controls (graphs) put each value on its own line.
                let mut lines = f.value.split(" / ");
                if let Some(first) = lines.next() {
                    txt(&self.pt, tl(0.0, 18.0), first, 17.0, th.oled_ink);
                }
                if let Some(second) = lines.next() {
                    txt(&self.pt, tl(0.0, 44.0), second, 17.0, th.oled_ink);
                }
                match f.kind {
                    FocusKind::Plain => {}
                    FocusKind::Wave(i) => {
                        let vals = self.osc_wave(p, i, 160);
                        self.wave_line(&vals, graph, 1.0, th.oled_ink, None);
                    }
                    FocusKind::Partials(i) => self.partial_graph(p, i, graph),
                    FocusKind::Lfo(i) => self.lfo_graph(p, i, graph),
                }
                self.st.focus = Some(f);
            }
            None => {
                let v = &self.st.visuals;
                let active = v.active_voices.load(Relaxed);
                let cap = v.voice_capacity.load(Relaxed);
                txt(&self.pt, tl(0.0, 0.0), "VOICES", 8.5, th.oled_dim);
                txt(&self.pt, tl(0.0, 18.0), &format!("{active} / {cap}"), 17.0, th.oled_ink);
                txt(&self.pt, tl(146.0, 0.0), "OSC MIX", 8.5, th.oled_dim);
                let a = self.osc_wave(p, 0, 160);
                let b = self.osc_wave(p, 1, 160);
                let (la, lb) = (self.plain(&p.osc1.level), self.plain(&p.osc2.level));
                let mix: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x * la + y * lb).collect();
                self.wave_line(&mix, graph, 1.0, th.oled_ink, None);
            }
        }

        // Footer with the voice settings, like soft key legends.
        let footer = format!(
            "{} | UNI {} | {} V | CHO {} | ANALOG {}",
            self.value_text(&p.filter.mode).to_uppercase(),
            self.plain(&p.voice.unison),
            self.plain(&p.voice.voice_count).count(),
            self.value_text(&p.chorus.mode).to_uppercase(),
            self.value_text(&p.voice.analog),
        );
        let footer_y = h - 28.0;
        self.pt.line_segment(
            [tl(0.0, footer_y - 4.0), tl(w - 16.0, footer_y - 4.0)],
            Stroke::new(0.6 * cv.s, th.oled_dim),
        );
        txt(&self.pt, tl(0.0, footer_y), &footer, 7.5, th.oled_dim);
    }

    fn osc_params(p: &SynthParams, i: usize) -> &OscParams {
        if i == 0 {
            &p.osc1
        } else {
            &p.osc2
        }
    }

    /// Per-partial gains after the macros, at a nominal pitch so the Nyquist
    /// fade does not confuse the picture.
    fn partial_gains(&mut self, o: &OscParams) -> [f32; N_PARTIALS] {
        let mut raw = [0.0_f32; N_PARTIALS];
        for (k, slot) in raw.iter_mut().enumerate() {
            *slot = self.plain(&o.partials[k].amp);
        }
        osc::partial_gains(
            &raw,
            self.plain(&o.tilt),
            self.plain(&o.partial_decay),
            self.plain(&o.odd_even),
            DISPLAY_HZ,
            20_000.0,
        )
    }

    /// Partial levels after the macros, peak normalised to 1.
    fn resolved_partials(&mut self, o: &OscParams) -> [f32; N_PARTIALS] {
        let gains = self.partial_gains(o);
        let peak = gains.iter().fold(1.0e-4_f32, |a, &b| a.max(b.abs()));
        gains.map(|g| (g / peak).abs())
    }

    /// `n` samples over two cycles of one oscillator as it sounds: waveform,
    /// pulse width, additive blend and the FM operator with its feedback.
    /// Uses the same DSP functions as the voice, at [`DISPLAY_HZ`].
    fn osc_wave(&mut self, p: &SynthParams, i: usize, n: usize) -> Vec<f32> {
        let o = Self::osc_params(p, i);
        let wave = self.plain(&o.wave);
        let width = self.plain(&o.pulse_width);
        let blend = self.plain(&o.blend);
        let gains = self.partial_gains(o);
        let fm = &p.fm;
        let mod_hz = fm::modulator_hz(
            self.plain(&fm.mode),
            self.plain(&fm.ratio),
            self.plain(&fm.ratio_fine),
            self.plain(&fm.fixed_hz),
            DISPLAY_HZ,
        );
        let index = self.plain(if i == 0 { &fm.index1 } else { &fm.index2 });
        let feedback = fm::feedback_cycles(self.plain(&fm.feedback));

        let dt = 2.0 / n as f32;
        let mut modulator = fm::ModOsc::default();
        modulator.inc = mod_hz / DISPLAY_HZ * dt;
        // Let the feedback path settle before the visible part starts.
        for _ in 0..n {
            modulator.next(feedback);
        }
        modulator.reset(0.0);
        (0..n)
            .map(|k| {
                let pm = index * modulator.next(feedback);
                let t = osc::wrap01(k as f32 * dt + pm);
                let sub = osc::subtractive(wave, t, dt, width);
                let add = osc::additive(t, &gains);
                sub + (add - sub) * blend
            })
            .collect()
    }

    /// Draw samples peak normalised and scaled by `amplitude`, optionally with
    /// a fill towards the centre line.
    fn wave_line(&self, vals: &[f32], rect: Rect, amplitude: f32, color: Color32, fill: Option<Color32>) {
        let peak = vals.iter().fold(1.0e-3_f32, |m, v| m.max(v.abs()));
        let n = vals.len().max(2);
        let mid = rect.center().y;
        let pts: Vec<Pos2> = vals
            .iter()
            .enumerate()
            .map(|(k, v)| {
                pos2(
                    rect.left() + rect.width() * k as f32 / (n - 1) as f32,
                    mid - v / peak * amplitude * rect.height() * 0.5,
                )
            })
            .collect();
        if let Some(fill) = fill {
            self.fill_under(&pts, mid, fill);
        }
        self.pt.add(Shape::line(pts, Stroke::new(1.6 * self.cv.s, color)));
    }

    /// The oscillator's wave on the panel. Height follows the level fader, so
    /// a muted oscillator flattens out.
    fn wave_display(&mut self, p: &SynthParams, i: usize, rect: Rect) {
        let s = self.cv.s;
        self.graph_bg(rect);
        let inner = rect.shrink2(vec2(8.0, 10.0) * s);
        self.pt.line_segment(
            [pos2(inner.left(), inner.center().y), pos2(inner.right(), inner.center().y)],
            Stroke::new(0.8 * s, self.th.scale),
        );
        let level = self.plain(&Self::osc_params(p, i).level);
        let db = 20.0 * level.max(1.0e-6).log10();
        let amplitude = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
        let vals = self.osc_wave(p, i, 200);
        self.wave_line(&vals, inner, amplitude, self.accent, Some(self.accent.gamma_multiply(0.18)));
    }

    fn partial_graph(&mut self, p: &SynthParams, i: usize, rect: Rect) {
        let o = Self::osc_params(p, i);
        let resolved = self.resolved_partials(o);
        let w = rect.width() / N_PARTIALS as f32;
        for (k, level) in resolved.iter().enumerate() {
            let raw = self.shown(&o.partials[k].amp);
            let x = rect.left() + k as f32 * w;
            let bar = |l: f32| {
                Rect::from_min_max(
                    pos2(x + w * 0.15, rect.bottom() - l * rect.height()),
                    pos2(x + w * 0.85, rect.bottom()),
                )
            };
            self.pt.rect_stroke(
                bar(raw),
                CornerRadius::ZERO,
                Stroke::new(0.6 * self.cv.s, self.th.oled_dim),
                StrokeKind::Inside,
            );
            self.pt
                .rect_filled(bar(*level), CornerRadius::ZERO, self.th.oled_ink);
        }
    }

    /// Filter response with a cutoff / resonance handle. Drag horizontally for
    /// cutoff (on a log axis) and vertically for resonance; double-click
    /// resets both, the wheel moves the cutoff. While notes sound, a second
    /// trace shows the cutoff actually reaching the most recent voice.
    fn filter_graph(&mut self, p: &SynthParams, rect: Rect) {
        let s = self.cv.s;
        let f = &p.filter;
        let span = (F_MAX / F_MIN).ln();
        let t_of = |hz: f32| (hz.max(F_MIN) / F_MIN).ln() / span;
        let hz_of = |t: f32| F_MIN * (t.clamp(0.0, 1.0) * span).exp();

        let inner = rect.shrink2(vec2(6.0, 8.0) * s);
        let id = Id::new("filter-graph");
        let resp = self.ui.interact(rect, id, Sense::click_and_drag());
        self.register_param(&f.cutoff, ControlKind::Graph, rect);
        if resp.drag_started() {
            let start = (t_of(self.plain(&f.cutoff)), self.shown(&f.resonance));
            self.ui.data_mut(|d| d.insert_temp(id, start));
            self.set.begin_set_parameter(&f.cutoff);
            self.set.begin_set_parameter(&f.resonance);
        }
        if resp.dragged() {
            let d = self.drag_offset();
            let current = (t_of(self.plain(&f.cutoff)), self.shown(&f.resonance));
            let (t0, r0): (f32, f32) = self.ui.data(|dm| dm.get_temp(id)).unwrap_or(current);
            // Mapped to the inner area, so the handle stays under the pointer.
            let t = (t0 + d.x / inner.width()).clamp(0.0, 1.0);
            let r = (r0 - d.y / inner.height()).clamp(0.0, 1.0);
            self.send(&f.cutoff, f.cutoff.preview_normalized(hz_of(t)));
            self.send(&f.resonance, r);
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(&f.cutoff);
            self.set.end_set_parameter(&f.resonance);
        }
        if resp.double_clicked() {
            self.change(&f.cutoff, f.cutoff.default_normalized_value());
            self.change(&f.resonance, f.resonance.default_normalized_value());
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                let t = t_of(self.plain(&f.cutoff)) + scroll * 0.0008;
                self.change(&f.cutoff, f.cutoff.preview_normalized(hz_of(t)));
            }
        }
        let active = resp.hovered() || resp.dragged();
        if active {
            let value = format!("{} / {}", self.value_text(&f.cutoff), self.value_text(&f.resonance));
            self.set_focus("CUTOFF / RESONANCE", value, FocusKind::Plain);
        }

        // Drawing
        self.graph_bg(rect);
        for hz in [50.0_f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
            let x = inner.left() + inner.width() * t_of(hz);
            let decade = hz == 100.0 || hz == 1000.0 || hz == 10_000.0;
            self.pt.line_segment(
                [pos2(x, inner.top()), pos2(x, inner.bottom())],
                Stroke::new((if decade { 1.0 } else { 0.6 }) * s, self.th.scale),
            );
        }
        let sample_rate = self.st.visuals.sample_rate.load(Relaxed).max(8000.0);
        let mode: FilterMode = self.plain(&f.mode);
        let cutoff = self.plain(&f.cutoff);
        let reso = self.plain(&f.resonance);
        let trace = |cutoff: f32, reso: f32| -> Vec<Pos2> {
            (0..120)
                .map(|i| {
                    let t = i as f32 / 119.0;
                    let mag = filter::magnitude(mode, cutoff, reso, hz_of(t), sample_rate);
                    let db = 20.0 * mag.max(1.0e-5).log10();
                    // +18 dB at the top for resonant peaks, -48 dB at the bottom.
                    let y = (1.0 - (db + 48.0) / 66.0).clamp(0.0, 1.0);
                    pos2(inner.left() + inner.width() * t, inner.top() + inner.height() * y)
                })
                .collect()
        };
        let pts = trace(cutoff, reso);
        self.fill_under(&pts, inner.bottom(), self.accent.gamma_multiply(0.28));
        self.pt.add(Shape::line(pts, Stroke::new(1.8 * s, self.accent)));
        if self.st.visuals.active_voices.load(Relaxed) > 0 {
            let live = self.st.visuals.filter_cutoff_hz.load(Relaxed).clamp(F_MIN, F_MAX);
            let live_reso = self.st.visuals.filter_resonance.load(Relaxed);
            self.pt.add(Shape::line(
                trace(live, live_reso),
                Stroke::new(1.0 * s, self.th.drag_line.gamma_multiply(0.7)),
            ));
        }
        let hx = inner.left() + inner.width() * t_of(cutoff);
        let hy = inner.bottom() - reso * inner.height();
        self.pt.line_segment(
            [pos2(hx, inner.top()), pos2(hx, inner.bottom())],
            Stroke::new(1.0 * s, self.accent.gamma_multiply(0.6)),
        );
        self.handle_square(pos2(hx, hy), active);
    }

    /// Envelope as a filled shape with three handles: attack time (top),
    /// decay time and sustain level (corner), release time (end). A dot rides
    /// along it for the amp and filter envelopes while notes sound.
    fn env_graph(&mut self, env: &AdsrParams, which: usize, rect: Rect) {
        let s = self.cv.s;
        self.graph_bg(rect);
        let inner = rect.shrink2(vec2(8.0, 8.0) * s);
        let (w, h) = (inner.width(), inner.height());
        let zone = 0.3 * w;
        let sus_end = inner.left() + 0.66 * w;
        let rel_zone = inner.right() - sus_end;
        let y_of = |l: f32| inner.bottom() - l * h;

        let na = self.shown(&env.attack);
        let nd = self.shown(&env.decay);
        let sus = self.plain(&env.sustain);
        let nr = self.shown(&env.release);
        let xa = inner.left() + na * zone;
        let xd = xa + nd * zone;
        let xr = sus_end + nr * rel_zone;

        // Release first so the attack and decay handles win where they overlap.
        let rel = self.env_handle(&env.release, None, pos2(xr, y_of(0.0)), rel_zone, h);
        let att = self.env_handle(&env.attack, None, pos2(xa, y_of(1.0)), zone, h);
        let dec = self.env_handle(&env.decay, Some(&env.sustain), pos2(xd, y_of(sus)), zone, h);

        // Shape, matching the RC segments of `env::Adsr`.
        let curve = self.plain(&env.curve);
        let ra = 10.0_f32.powf(2.0 - 5.0 * curve);
        let rdr = 10.0_f32.powf(1.0 - 5.0 * curve);
        let fall = |u: f32, r: f32| (1.0 + r) * (r / (1.0 + r)).powf(u) - r;
        let steps = 20;
        let mut pts = Vec::with_capacity(4 * steps + 4);
        for k in 0..=steps {
            let u = k as f32 / steps as f32;
            pts.push(pos2(inner.left() + u * (xa - inner.left()), y_of(1.0 - fall(u, ra))));
        }
        for k in 1..=steps {
            let u = k as f32 / steps as f32;
            pts.push(pos2(xa + u * (xd - xa), y_of(sus + (1.0 - sus) * fall(u, rdr))));
        }
        pts.push(pos2(sus_end, y_of(sus)));
        for k in 1..=steps {
            let u = k as f32 / steps as f32;
            pts.push(pos2(sus_end + u * (xr - sus_end), y_of(sus * fall(u, rdr))));
        }
        pts.push(pos2(inner.right(), y_of(0.0)));

        self.fill_under(&pts, inner.bottom(), self.accent.gamma_multiply(0.55));
        // Stage dividers cut through the fill, as on a hardware envelope display.
        for (x, l) in [(xa, 1.0), (xd, sus), (sus_end, sus)] {
            self.pt.line_segment(
                [pos2(x, y_of(l)), pos2(x, inner.bottom())],
                Stroke::new(2.0 * s, self.th.slot.gamma_multiply(0.8)),
            );
        }
        self.pt.add(Shape::line(pts, Stroke::new(1.6 * s, self.accent)));

        let v = &self.st.visuals;
        let live = match which {
            0 => Some((v.amp_env_level.load(Relaxed), v.amp_env_stage.load(Relaxed))),
            1 => Some((v.filter_env_level.load(Relaxed), v.filter_env_stage.load(Relaxed))),
            _ => None,
        };
        if let Some((level, stage)) = live.filter(|(_, stage)| *stage != 0) {
            // Place the dot from the stage plus how far the level has travelled within it.
            let x = match stage {
                1 => inner.left() + (xa - inner.left()) * level,
                2 => {
                    let span = (1.0 - sus).max(1.0e-3);
                    xa + (xd - xa) * ((1.0 - level) / span).clamp(0.0, 1.0)
                }
                3 => (xd + sus_end) * 0.5,
                _ => sus_end + (xr - sus_end) * (1.0 - (level / sus.max(1.0e-3)).clamp(0.0, 1.0)),
            };
            let at = pos2(x, y_of(level.clamp(0.0, 1.0)));
            self.pt.circle_filled(at, 4.0 * s, self.th.drag_line);
            self.pt.circle_stroke(at, 4.0 * s, Stroke::new(1.2 * s, self.th.slot));
        }

        self.handle_square(pos2(xr, y_of(0.0)), rel);
        self.handle_square(pos2(xa, y_of(1.0)), att);
        self.handle_square(pos2(xd, y_of(sus)), dec);
    }

    /// Pointer movement since the button went down, scaled down with Shift.
    /// Graph handles use this rather than per-frame deltas so the movement
    /// before egui recognises the drag is not lost; their spans are short.
    fn drag_offset(&self) -> Vec2 {
        self.ui.input(|i| {
            let fine = if i.modifiers.shift { 0.15 } else { 1.0 };
            match (i.pointer.press_origin(), i.pointer.interact_pos()) {
                (Some(a), Some(b)) => (b - a) * fine,
                _ => Vec2::ZERO,
            }
        })
    }

    /// Mouse handling for one envelope handle: horizontal drag moves `x`
    /// across `span_x` screen pixels, vertical drag moves the optional `y`.
    /// Double-click resets, the wheel moves `x`. Returns whether the handle
    /// is hovered or dragged.
    fn env_handle(
        &mut self,
        x: &FloatParam,
        y: Option<&FloatParam>,
        at: Pos2,
        span_x: f32,
        span_y: f32,
    ) -> bool {
        let s = self.cv.s;
        let id = Id::new(("env-handle", x.as_ptr()));
        let hit = Rect::from_center_size(at, vec2(18.0, 18.0) * s);
        let resp = self.ui.interact(hit, id, Sense::click_and_drag());
        self.register_param(x, ControlKind::Handle, hit);

        if resp.drag_started() {
            let start = (self.shown(x), y.map(|y| self.shown(y)).unwrap_or(0.0));
            self.ui.data_mut(|d| d.insert_temp(id, start));
            self.set.begin_set_parameter(x);
            if let Some(y) = y {
                self.set.begin_set_parameter(y);
            }
        }
        if resp.dragged() {
            let d = self.drag_offset();
            let current = (self.shown(x), y.map(|y| self.shown(y)).unwrap_or(0.0));
            let (x0, y0): (f32, f32) = self.ui.data(|dm| dm.get_temp(id)).unwrap_or(current);
            let vx = (x0 + d.x / span_x).clamp(0.0, 1.0);
            let vy = (y0 - d.y / span_y).clamp(0.0, 1.0);
            self.send(x, vx);
            if let Some(y) = y {
                self.send(y, vy);
            }
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(x);
            if let Some(y) = y {
                self.set.end_set_parameter(y);
            }
        }
        if resp.double_clicked() {
            self.change(x, x.default_normalized_value());
            if let Some(y) = y {
                self.change(y, y.default_normalized_value());
            }
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                let v = self.shown(x) + scroll * 0.0015;
                self.change(x, v);
            }
        }
        let active = resp.hovered() || resp.dragged();
        if active {
            let (name, value) = match y {
                Some(y) => (
                    format!("{} / {}", x.name(), y.name()),
                    format!("{} / {}", self.value_text(x), self.value_text(y)),
                ),
                None => (x.name().to_owned(), self.value_text(x)),
            };
            self.set_focus(&name, value, FocusKind::Plain);
        }
        active
    }

    /// Two cycles of the selected shape, with the live output as a dot on the right.
    fn lfo_graph(&mut self, p: &SynthParams, i: usize, rect: Rect) {
        let lfo = if i == 0 { &p.lfo1 } else { &p.lfo2 };
        let shape: LfoShape = self.plain(&lfo.shape);
        let index = shape.to_index();
        let pts = sample_icon(rect.shrink2(vec2(0.0, 2.0 * self.cv.s)), 2.0, |ph| {
            lfo_shape(index, ph)
        });
        self.pt
            .add(Shape::line(pts, Stroke::new(1.2 * self.cv.s, self.th.oled_ink)));
        let live = match i {
            0 => self.st.visuals.lfo1_value.load(Relaxed),
            _ => self.st.visuals.lfo2_value.load(Relaxed),
        };
        let y = rect.center().y - live.clamp(-1.0, 1.0) * (rect.height() * 0.5 - 2.0 * self.cv.s);
        self.pt
            .circle_filled(pos2(rect.right() + 4.0 * self.cv.s, y), 2.5 * self.cv.s, self.accent);
    }
}

/// Polyline of `f(phase)` across `rect`, `cycles` periods wide, `f` in -1..1.
fn sample_icon(rect: Rect, cycles: f32, f: impl Fn(f32) -> f32) -> Vec<Pos2> {
    let n = 48;
    (0..=n)
        .map(|k| {
            let t = k as f32 / n as f32;
            // The last point ends the final cycle instead of wrapping to its start.
            let ph = if k == n { 0.9999 } else { (t * cycles).fract() };
            let v = f(ph);
            pos2(
                rect.left() + rect.width() * t,
                rect.center().y - v * rect.height() * 0.5,
            )
        })
        .collect()
}

/// Ideal waveform for the stepper icons.
fn naive_wave(wave: Waveform, ph: f32) -> f32 {
    match wave {
        Waveform::Sine => (ph * TAU).sin(),
        Waveform::Triangle => 1.0 - 4.0 * (ph - 0.5).abs(),
        Waveform::Saw => 1.0 - 2.0 * ph,
        Waveform::Pulse => {
            if ph < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

/// LFO shape by enum index, in the order of `LfoShape`.
fn lfo_shape(index: usize, ph: f32) -> f32 {
    match index {
        0 => (ph * TAU).sin(),
        1 => 1.0 - 4.0 * (ph - 0.5).abs(),
        2 => 2.0 * ph - 1.0,
        3 => 1.0 - 2.0 * ph,
        4 => {
            if ph < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        // Sample and hold: four held steps per cycle.
        5 => [0.6, -0.4, 0.9, -0.8][((ph * 4.0) as usize).min(3)],
        // Smooth random: a few incommensurate sines.
        _ => 0.6 * (ph * TAU).sin() + 0.35 * (ph * TAU * 2.7 + 1.0).sin(),
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
