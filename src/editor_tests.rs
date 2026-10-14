//! Headless GUI tests: the editor runs in a real egui context with simulated
//! mouse input against a mock host that applies parameter changes with a delay,
//! like nih-plug's CLAP wrapper does. Controls are addressed by parameter ID.

use super::*;
use crate::visuals::ScopeInput;
use nih_plug::wrapper::state::PluginState;
use nih_plug_egui::egui::{Event, Modifiers, PointerButton, RawInput, Vec2};

const SCREEN: Vec2 = Vec2::new(BW, BH);

#[derive(Default)]
struct MockHost {
    /// Values the "plugin" has applied (what the GUI reads back).
    store: PreviewStore,
    /// Values sent by the GUI but not applied yet.
    queue: Mutex<Vec<(ParamPtr, f32)>>,
    open_gestures: Mutex<HashMap<ParamPtr, i32>>,
    errors: Mutex<Vec<String>>,
}

impl MockHost {
    fn flush(&self) {
        let mut store = self.store.lock().unwrap();
        for (ptr, v) in self.queue.lock().unwrap().drain(..) {
            store.insert(ptr, v);
        }
    }
}

impl GuiContext for MockHost {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }
    fn request_resize(&self) -> bool {
        false
    }
    unsafe fn raw_begin_set_parameter(&self, param: ParamPtr) {
        let mut g = self.open_gestures.lock().unwrap();
        let n = g.entry(param).or_default();
        if *n != 0 {
            self.errors
                .lock()
                .unwrap()
                .push(format!("nested gesture: {}", param.name()));
        }
        *n += 1;
    }
    unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
        if self.open_gestures.lock().unwrap().get(&param).copied().unwrap_or(0) != 1 {
            self.errors
                .lock()
                .unwrap()
                .push(format!("set outside gesture: {}", param.name()));
        }
        self.queue.lock().unwrap().push((param, normalized));
    }
    unsafe fn raw_end_set_parameter(&self, param: ParamPtr) {
        let mut g = self.open_gestures.lock().unwrap();
        let n = g.entry(param).or_default();
        *n -= 1;
        if *n != 0 {
            self.errors
                .lock()
                .unwrap()
                .push(format!("unbalanced end: {}", param.name()));
        }
    }
    fn get_state(&self) -> PluginState {
        unimplemented!()
    }
    fn set_state(&self, _state: PluginState) {}
}

struct Harness {
    ctx: egui::Context,
    params: SynthParams,
    host: MockHost,
    state: EditorState,
    time: f64,
    frame: u64,
    /// Host applies queued values every `latency` frames.
    latency: u64,
    events: Vec<Event>,
    pointer: Pos2,
}

impl Harness {
    fn new(latency: u64) -> Self {
        let ctx = egui::Context::default();
        setup_style(&ctx);
        let host = MockHost::default();
        let params = SynthParams::default();
        let (_input, output) = ScopeInput::new();
        let state = EditorState::new(
            Arc::new(VisualState::default()),
            Arc::new(Mutex::new(output)),
        )
        .with_preview_store(host.store.clone());
        let mut h = Self {
            ctx,
            params,
            host,
            state,
            time: 0.0,
            frame: 0,
            latency,
            events: Vec::new(),
            pointer: pos2(0.0, 0.0),
        };
        h.run(2);
        h
    }

    fn run(&mut self, frames: usize) {
        for _ in 0..frames {
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                time: Some(self.time),
                events: std::mem::take(&mut self.events),
                ..Default::default()
            };
            let setter = ParamSetter::new(&self.host);
            let (params, state) = (&self.params, &mut self.state);
            let _ = self.ctx.run(raw, |ctx| root(ctx, params, &setter, state));
            self.time += 1.0 / 60.0;
            self.frame += 1;
            if self.latency > 0 && self.frame.is_multiple_of(self.latency) {
                self.host.flush();
            }
        }
    }

    fn settle(&mut self) {
        self.run(3);
        self.host.flush();
        self.run(2);
    }

    fn move_to(&mut self, p: Pos2) {
        self.pointer = p;
        self.events.push(Event::PointerMoved(p));
    }

    fn button(&mut self, pressed: bool) {
        self.events.push(Event::PointerButton {
            pos: self.pointer,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        });
    }

    fn click(&mut self, p: Pos2) {
        self.move_to(p);
        self.run(1);
        self.button(true);
        self.run(1);
        self.button(false);
        self.run(1);
        // Make sure the next click is not seen as a double click.
        self.time += 0.4;
    }

    fn drag(&mut self, from: Pos2, delta: Vec2, steps: usize) {
        self.move_to(from);
        self.run(1);
        self.button(true);
        self.run(1);
        for k in 1..=steps {
            self.move_to(from + delta * (k as f32 / steps as f32));
            self.run(1);
        }
        self.button(false);
        self.run(1);
        self.move_to(pos2(2.0, 2.0));
        self.run(1);
        self.time += 0.4;
    }

    fn scroll(&mut self, at: Pos2, amount: f32) {
        self.move_to(at);
        self.run(1);
        self.events.push(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, amount),
            modifiers: Modifiers::default(),
        });
        self.run(1);
        self.move_to(pos2(2.0, 2.0));
        self.run(1);
    }

    fn control(&self, name: &str) -> ControlInfo {
        self.state
            .controls
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("control {name} not found"))
            .clone()
    }

    fn controls(&self, kind: ControlKind) -> Vec<ControlInfo> {
        self.state
            .controls
            .iter()
            .filter(|c| c.kind == kind)
            .cloned()
            .collect()
    }

    fn ptr(&self, id: &str) -> ParamPtr {
        self.params
            .param_map()
            .into_iter()
            .find(|(pid, _, _)| pid == id)
            .map(|(_, ptr, _)| ptr)
            .unwrap_or_else(|| panic!("param {id} not found"))
    }

    /// Applied normalized value of a parameter.
    fn value(&self, id: &str) -> f32 {
        let ptr = self.ptr(id);
        if let Some(v) = self.host.store.lock().unwrap().get(&ptr) {
            return *v;
        }
        unsafe { ptr.default_normalized_value() }
    }

    fn step_count(&self, id: &str) -> Option<usize> {
        unsafe { self.ptr(id).step_count() }
    }

    fn assert_clean(&self) {
        let errors = self.host.errors.lock().unwrap();
        assert!(errors.is_empty(), "host protocol errors: {errors:?}");
        let open: Vec<_> = self
            .host
            .open_gestures
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, n)| **n != 0)
            .map(|(k, _)| unsafe { k.name() }.to_owned())
            .collect();
        assert!(open.is_empty(), "gestures left open: {open:?}");
    }
}

fn canvas_scale(c: &ControlInfo, canvas_width: f32) -> f32 {
    c.rect.width() / canvas_width
}

/// Fader slot travel in canvas pixels.
const TRAVEL: f32 = 110.0;

#[test]
fn every_control_is_registered() {
    let h = Harness::new(3);
    assert_eq!(h.controls(ControlKind::Fader).len(), 51);
    assert_eq!(h.controls(ControlKind::Drawbar).len(), 16);
    assert_eq!(h.controls(ControlKind::Stepper).len(), 11);
    assert_eq!(h.controls(ControlKind::Handle).len(), 9);
    assert_eq!(h.controls(ControlKind::Graph).len(), 1);
    // Every registered parameter control resolves to a real parameter ID.
    for c in &h.state.controls {
        if c.kind != ControlKind::Button {
            h.ptr(&c.name);
        }
    }
}

/// Every parameter except the unused LFO sync settings and the per-voice-only
/// LFO 2 delay and fade has a control. Sustain and resonance are the second
/// axis of a handle or graph.
#[test]
fn every_used_parameter_has_a_control() {
    let h = Harness::new(3);
    let seen: std::collections::HashSet<String> =
        h.state.controls.iter().map(|c| c.name.clone()).collect();
    let missing: Vec<String> = h
        .params
        .param_map()
        .into_iter()
        .map(|(id, _, _)| id)
        .filter(|id| !seen.contains(id))
        .collect();
    assert_eq!(
        missing,
        [
            "flt_reso", "ae_s", "fe_s", "me_s", "l1_sync", "l1_div", "l2_sync", "l2_div",
            "l2_delay", "l2_fade"
        ],
        "parameters without a control"
    );
}

/// Every fader follows a drag by the full distance, even though the host applies values late.
#[test]
fn faders_follow_drag_with_host_latency() {
    let mut h = Harness::new(4);
    let mut faders = h.controls(ControlKind::Fader);
    faders.extend(h.controls(ControlKind::Drawbar));
    for c in faders {
        let width = if c.kind == ControlKind::Drawbar { 24.0 } else { 28.0 };
        let s = canvas_scale(&c, width);
        let before = h.value(&c.name);
        // Move away from the nearer end so the change is not clipped.
        let up = before < 0.5;
        let px = 0.4 * TRAVEL * s;
        h.drag(c.rect.center(), vec2(0.0, if up { -px } else { px }), 12);
        h.settle();
        let after = h.value(&c.name);
        let change = after - before;
        match h.step_count(&c.name) {
            None => {
                let expected = if up { 0.4 } else { -0.4 };
                assert!(
                    (change - expected).abs() < 0.08,
                    "{}: moved {change:.3}, expected {expected:.3}",
                    c.name
                );
            }
            Some(_) => assert!(
                if up { change > 0.2 } else { change < -0.2 },
                "{}: stepped fader moved {change:.3}",
                c.name
            ),
        }
    }
    h.assert_clean();
}

/// Without any host updates during the drag, the fader still ends at the dragged position.
#[test]
fn fader_does_not_depend_on_read_back() {
    let mut h = Harness::new(0);
    let c = h.control("o1_fine");
    let s = canvas_scale(&c, 28.0);
    let before = h.value("o1_fine");
    h.drag(c.rect.center(), vec2(0.0, 0.3 * TRAVEL * s), 15);
    h.host.flush();
    let after = h.value("o1_fine");
    assert!((after - (before - 0.3)).abs() < 0.06, "{before} -> {after}");
    h.assert_clean();
}

/// Same display name on both oscillators, different parameters.
#[test]
fn oscillator_controls_are_independent() {
    let mut h = Harness::new(2);
    let a = h.value("o1_fine");
    let b = h.value("o2_fine");
    let c = h.control("o2_fine");
    let s = canvas_scale(&c, 28.0);
    h.drag(c.rect.center(), vec2(0.0, -0.3 * TRAVEL * s), 10);
    h.settle();
    assert_eq!(h.value("o1_fine"), a);
    assert!(h.value("o2_fine") > b + 0.2);
    h.assert_clean();
}

#[test]
fn double_click_resets_and_scroll_changes() {
    let mut h = Harness::new(2);
    let c = h.control("flt_drive");
    let default = h.value("flt_drive");
    h.scroll(c.rect.center(), 60.0);
    h.settle();
    let scrolled = h.value("flt_drive");
    assert!(scrolled > default + 0.02, "scroll: {default} -> {scrolled}");

    h.move_to(c.rect.center());
    h.run(1);
    for _ in 0..2 {
        h.button(true);
        h.run(1);
        h.button(false);
        h.run(1);
    }
    h.settle();
    assert!((h.value("flt_drive") - default).abs() < 1e-4);
    h.assert_clean();
}

/// One wheel notch moves an integer fader by at least one step.
#[test]
fn scroll_steps_integer_fader() {
    let mut h = Harness::new(2);
    let before = h.value("vc_bend");
    let c = h.control("vc_bend");
    h.scroll(c.rect.center(), 1.0);
    h.settle();
    let steps = h.step_count("vc_bend").unwrap() as f32;
    assert!((h.value("vc_bend") - (before + 1.0 / steps)).abs() < 1e-4);
    h.assert_clean();
}

/// Clicking the top and bottom detent of every stepped fader selects the end values.
#[test]
fn steppers_click_positions() {
    let mut h = Harness::new(3);
    for c in h.controls(ControlKind::Stepper) {
        let s = canvas_scale(&c, 54.0);
        let top = pos2(c.rect.center().x, c.rect.top() + 16.0 * s);
        let bottom = pos2(c.rect.center().x, c.rect.bottom() - 16.0 * s);
        h.click(top);
        h.settle();
        let v = h.value(&c.name);
        assert!((v - 1.0).abs() < 1e-4, "{} top: {v}", c.name);
        h.click(bottom);
        h.settle();
        let v = h.value(&c.name);
        assert!(v.abs() < 1e-4, "{} bottom: {v}", c.name);
    }
    h.assert_clean();
}

#[test]
fn stepper_drag_moves_through_positions() {
    let mut h = Harness::new(5);
    let c = h.control("flt_mode");
    let s = canvas_scale(&c, 54.0);
    let bottom = pos2(c.rect.center().x, c.rect.bottom() - 16.0 * s);
    h.click(bottom);
    h.settle();
    assert_eq!(h.value("flt_mode"), 0.0);
    // Two detents up (110 px travel / 6 steps per detent).
    h.drag(bottom, vec2(0.0, -2.0 * TRAVEL / 6.0 * s), 20);
    h.settle();
    assert!((h.value("flt_mode") - 2.0 / 6.0).abs() < 1e-4, "{}", h.value("flt_mode"));
    h.assert_clean();
}

/// Envelope handles move their time parameters horizontally, and the decay
/// handle moves sustain vertically.
#[test]
fn envelope_handles_drag_times_and_sustain() {
    let mut h = Harness::new(3);
    let attack = h.control("ae_a");
    // Graph is 200 canvas px wide with 8 px padding; the attack zone is 30 %.
    let s = attack.rect.width() / 18.0;
    let zone = 0.3 * (200.0 - 16.0) * s;
    let before = h.value("ae_a");
    h.drag(attack.rect.center(), vec2(0.25 * zone, 0.0), 10);
    h.settle();
    let after = h.value("ae_a");
    assert!((after - before - 0.25).abs() < 0.03, "attack {before} -> {after}");

    let decay = h.control("ae_d");
    let (d0, s0) = (h.value("ae_d"), h.value("ae_s"));
    let height = (130.0 - 16.0) * s;
    h.drag(decay.rect.center(), vec2(0.2 * zone, -0.2 * height), 10);
    h.settle();
    assert!((h.value("ae_d") - d0 - 0.2).abs() < 0.03, "decay {d0} -> {}", h.value("ae_d"));
    assert!((h.value("ae_s") - s0 - 0.2).abs() < 0.03, "sustain {s0} -> {}", h.value("ae_s"));

    let release = h.control("me_r");
    let r0 = h.value("me_r");
    // The release zone is 34 % of the width.
    h.drag(release.rect.center(), vec2(-0.3 * zone, 0.0), 10);
    h.settle();
    let expected = r0 - 0.3 * 0.3 / 0.34;
    assert!((h.value("me_r") - expected).abs() < 0.03, "release {r0} -> {}", h.value("me_r"));

    // Double-click resets both parameters of the decay handle.
    let decay = h.control("ae_d");
    h.move_to(decay.rect.center());
    h.run(1);
    for _ in 0..2 {
        h.button(true);
        h.run(1);
        h.button(false);
        h.run(1);
    }
    h.settle();
    assert!((h.value("ae_d") - d0).abs() < 1e-4);
    assert!((h.value("ae_s") - s0).abs() < 1e-4);
    h.assert_clean();
}

/// Dragging the filter graph moves cutoff on a log axis and resonance vertically.
#[test]
fn filter_graph_drags_cutoff_and_resonance() {
    let mut h = Harness::new(3);
    let g = h.control("flt_cutoff");
    assert_eq!(g.kind, ControlKind::Graph);
    let hz = |h: &Harness| {
        let ptr = h.ptr("flt_cutoff");
        unsafe { ptr.preview_plain(h.value("flt_cutoff")) }
    };
    let before = hz(&h);
    let r0 = h.value("flt_reso");
    // One tenth of the width is one tenth of the three decades: about 2x.
    let inner_w = g.rect.width() * (220.0 - 12.0) / 220.0;
    let inner_h = g.rect.height() * (130.0 - 16.0) / 130.0;
    h.drag(g.rect.center(), vec2(-0.1 * inner_w, -0.3 * inner_h), 10);
    h.settle();
    let after = hz(&h);
    let expected = before / 1000.0_f32.powf(0.1);
    assert!((after / expected - 1.0).abs() < 0.08, "cutoff {before} -> {after}, expected {expected}");
    assert!((h.value("flt_reso") - r0 - 0.3).abs() < 0.03, "reso {r0} -> {}", h.value("flt_reso"));
    h.assert_clean();
}

/// The GUI keeps showing a value it sent while the host has not applied it yet.
#[test]
fn pending_value_is_displayed_until_applied() {
    let mut h = Harness::new(0);
    let c = h.control("o1_fine");
    let s = canvas_scale(&c, 28.0);
    h.drag(c.rect.center(), vec2(0.0, -20.0 * s), 6);
    let ptr = h.ptr("o1_fine");
    let pending = h.state.pending.get(&ptr).map(|p| p.value);
    assert!(pending.is_some(), "no pending value recorded");
    assert!(pending.unwrap() > h.value("o1_fine"));
    h.host.flush();
    h.run(2);
    assert!(!h.state.pending.contains_key(&ptr), "pending not cleared after apply");
}

/// Hovering a control puts its name and value on the OLED.
#[test]
fn hover_sets_focus() {
    let mut h = Harness::new(2);
    let c = h.control("o1_pw");
    h.move_to(c.rect.center());
    h.run(2);
    let focus = h.state.focus.as_ref().expect("no focus");
    assert_eq!(focus.name, "PULSE WIDTH");
    assert!(focus.kind == FocusKind::Wave(0));
}

/// Every theme chip switches the scheme, and every scheme renders all controls.
#[test]
fn theme_selector_switches_and_all_themes_render() {
    let mut h = Harness::new(2);
    let controls_before = h.state.controls.len();
    for (i, theme) in THEMES.iter().enumerate() {
        let chip = h.control(&format!("theme:{}", theme.name));
        h.click(chip.rect.center());
        h.run(2);
        assert_eq!(h.params.theme.load(Relaxed) as usize, i, "{}", theme.name);
        assert_eq!(h.state.texture.as_ref().map(|(t, _)| *t as usize), Some(i));
        assert_eq!(h.state.controls.len(), controls_before);
    }
    // Controls keep working after a theme switch.
    let c = h.control("o1_fine");
    let s = canvas_scale(&c, 28.0);
    let before = h.value("o1_fine");
    h.drag(c.rect.center(), vec2(0.0, 0.2 * TRAVEL * s), 10);
    h.settle();
    assert!(h.value("o1_fine") < before - 0.1);
    h.assert_clean();
}

