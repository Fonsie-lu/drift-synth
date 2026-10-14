//! Opens the editor in a standalone window without a plugin host. Parameter
//! changes go into a local store so every control can be operated and checked.
//! There is no audio, so the scope, meters and voice LEDs stay idle.
//!
//!   cargo run --release --example gui_preview
//!   DRIFT_THEME=0..3 cargo run --release --example gui_preview
use baseview::{Size, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use nih_plug::prelude::*;
use nih_plug::wrapper::state::PluginState;
use std::sync::{Arc, Mutex};
use synth::editor::{self, EditorState, PreviewStore};
use synth::params::SynthParams;
use synth::visuals::{ScopeInput, VisualState};

struct PreviewContext {
    store: PreviewStore,
}

impl GuiContext for PreviewContext {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }
    fn request_resize(&self) -> bool {
        false
    }
    unsafe fn raw_begin_set_parameter(&self, _param: ParamPtr) {}
    unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
        self.store.lock().unwrap().insert(param, normalized);
    }
    unsafe fn raw_end_set_parameter(&self, _param: ParamPtr) {}
    fn get_state(&self) -> PluginState {
        unimplemented!()
    }
    fn set_state(&self, _state: PluginState) {}
}

fn main() {
    let params = SynthParams::default();
    if let Some(i) = std::env::var("DRIFT_THEME").ok().and_then(|v| v.parse().ok()) {
        params.theme.store(i, std::sync::atomic::Ordering::Relaxed);
    }
    let (_scope_input, scope_output) = ScopeInput::new();
    let store = PreviewStore::default();
    let state = EditorState::new(
        Arc::new(VisualState::default()),
        Arc::new(Mutex::new(scope_output)),
    )
    .with_preview_store(store.clone());
    let settings = WindowOpenOptions {
        title: "Drift Preview".into(),
        size: Size::new(editor::WIDTH as f64, editor::HEIGHT as f64),
        scale: WindowScalePolicy::ScaleFactor(1.0),
        gl_config: Some(Default::default()),
    };
    let context = PreviewContext { store };
    EguiWindow::open_blocking(
        settings,
        GraphicsConfig::default(),
        state,
        |ctx, _queue, _state| editor::setup_style(ctx),
        move |ctx, _queue, state| {
            let setter = ParamSetter::new(&context);
            editor::root(ctx, &params, &setter, state);
        },
    );
}
