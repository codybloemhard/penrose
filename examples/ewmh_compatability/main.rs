//! penrose :: EWMH support
//!
//! It is possible to add EWMH support to penrose via an extension. This provides
//! information to external utilities such as panels and statusbars so that they
//! are able to interact with the window manager.
//!
//! `penrose::extensions::hooks::add_ewmh_hooks` can be used to compose the required
//! hooks into your existing Config before starting the window manager. If you want
//! to modify the support, each of the individual hooks can be found in
//! `penrose::extensions::hooks::ewmh`.
use penrose::{
    builtin::{
        actions::{
            exit,
            floating::{sink_focused, MouseDragHandler, MouseResizeHandler},
            log_current_state, modify_with, send_layout_message, spawn,
        },
        layout::{
            messages::{ExpandMain, IncMain, ShrinkMain},
            transformers::{Gaps, ReflectHorizontal, ReserveTop},
            MainAndStack,
        },
    },
    core::{
        bindings::{
            click_handler, parse_keybindings_with_xmodmap, KeyEventHandler, MouseEventHandler,
            MouseState,
        },
        layout::LayoutStack,
        Config, WindowManager,
    },
    extensions::{
        actions::toggle_fullscreen,
        hooks::{add_ewmh_hooks, SpawnOnStartup},
    },
    map, stack,
    x11rb::RustConn,
    Result,
};
use std::collections::HashMap;
use tracing_subscriber::{self, prelude::*};

fn raw_key_bindings() -> HashMap<String, Box<dyn KeyEventHandler<RustConn>>> {
    let mut raw_bindings = map! {
        map_keys: |k: &str| k.to_owned();

        "M-j" => modify_with(|cs| cs.focus_down()),
        "M-k" => modify_with(|cs| cs.focus_up()),
        "M-S-j" => modify_with(|cs| cs.swap_down()),
        "M-S-k" => modify_with(|cs| cs.swap_up()),
        "M-S-q" => modify_with(|cs| cs.kill_focused()),
        "M-Tab" => modify_with(|cs| cs.toggle_tag()),
        "M-bracketright" => modify_with(|cs| cs.next_screen()),
        "M-bracketleft" => modify_with(|cs| cs.previous_screen()),
        "M-grave" => modify_with(|cs| cs.next_layout()),
        "M-S-grave" => modify_with(|cs| cs.previous_layout()),
        "M-Up" => send_layout_message(|| IncMain(1)),
        "M-Down" => send_layout_message(|| IncMain(-1)),
        "M-Right" => send_layout_message(|| ExpandMain),
        "M-Left" => send_layout_message(|| ShrinkMain),
        "M-semicolon" => spawn("dmenu_run"),
        "M-S-s" => log_current_state(),
        "M-Return" => spawn("st"),
        "M-A-Escape" => exit(),

        "M-S-f" => toggle_fullscreen(),
    };

    for tag in &["1", "2", "3", "4", "5", "6", "7", "8", "9"] {
        raw_bindings.extend([
            (
                format!("M-{tag}"),
                modify_with(move |client_set| client_set.focus_tag(tag)),
            ),
            (
                format!("M-S-{tag}"),
                modify_with(move |client_set| client_set.move_focused_to_tag(tag)),
            ),
        ]);
    }

    raw_bindings
}

fn mouse_bindings() -> HashMap<MouseState, Box<dyn MouseEventHandler<RustConn>>> {
    use penrose::core::bindings::{
        ModifierKey::{Meta, Shift},
        MouseButton::{Left, Middle, Right},
    };

    map! {
        map_keys: |(button, modifiers)| MouseState { button, modifiers };

        (Left, vec![Shift, Meta]) => MouseDragHandler::boxed_default(),
        (Right, vec![Shift, Meta]) => MouseResizeHandler::boxed_default(),
        (Middle, vec![Shift, Meta]) => click_handler(sink_focused()),
    }
}

fn layouts() -> LayoutStack {
    let max_main = 1;
    let ratio = 0.6;
    let ratio_step = 0.1;
    let outer_px = 5;
    let inner_px = 5;
    let top_px = 18;

    stack!(
        MainAndStack::side(max_main, ratio, ratio_step),
        ReflectHorizontal::wrap(MainAndStack::side(max_main, ratio, ratio_step)),
        MainAndStack::bottom(max_main, ratio, ratio_step)
    )
    .map(|layout| ReserveTop::wrap(Gaps::wrap(layout, outer_px, inner_px), top_px))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("trace")
        .finish()
        .init();

    // This is all that is needed to add the required hooks to your config.
    // SpawnOnStartup is being used here to start polybar so that the EWMH support
    // can be demonstrated.
    let config = add_ewmh_hooks(Config {
        default_layouts: layouts(),
        startup_hook: Some(SpawnOnStartup::boxed("polybar")),
        ..Config::default()
    });

    let conn = RustConn::new()?;
    let key_bindings = parse_keybindings_with_xmodmap(raw_key_bindings())?;
    let wm = WindowManager::new(config, key_bindings, mouse_bindings(), conn)?;

    wm.run()
}
