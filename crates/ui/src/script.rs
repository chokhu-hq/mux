// dev aid: MUX_SCRIPT=path runs the steps in that file against the app, one per line, then quits.
// widgets are found by their label in egui's accessibility tree, so a script reads like what's on screen:
//
//   wait 1                 let things settle for a second
//   click Settings         click the widget labelled Settings, or containing it
//   click 400 300          click at a point
//   hover New agent        move the pointer over a widget
//   type hello there       type text into whatever has focus
//   key ctrl+k             press a key, with any of ctrl, shift, alt, cmd
//   scroll -300            scroll at the pointer
//   expect Connected       fail unless something labelled that is on screen
//   shot settings          save target/shots/settings.png, or under MUX_SCRIPT_OUT
//   labels                 print every label on screen, for writing the next step
//
// a step that can't be done is reported on stderr, and the app exits with status 1 once the script ends
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Context, Event, FullOutput, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2};

// time for animations to settle after each step, so the next one sees where things ended up
const STEP_PAUSE: Duration = Duration::from_millis(450);
const START_PAUSE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Wait(Duration),
    Click(Target),
    Hover(Target),
    Type(String),
    Key(Modifiers, Key),
    Scroll(f32),
    Expect(String),
    Shot(String),
    Labels,
}

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Label(String),
    Point(Pos2),
}

pub struct Script {
    steps: VecDeque<(usize, Step)>,
    // raw events waiting to go out, one batch per frame, so a press and its release land in different frames
    frames: VecDeque<Vec<Event>>,
    next: Instant,
    // the labelled widgets on screen last frame, with where they are
    widgets: Vec<(String, Rect)>,
    pointer: Pos2,
    shot_pending: bool,
    out: PathBuf,
    failures: Vec<String>,
    done: bool,
}

// installs the script named by MUX_SCRIPT, if there is one
pub fn install(ctx: &Context) {
    let Some(path) = std::env::var_os("MUX_SCRIPT") else {
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!(
                "MUX_SCRIPT: couldn't read {}: {err}",
                PathBuf::from(path).display()
            );
            std::process::exit(1);
        }
    };
    let steps = match parse(&text) {
        Ok(steps) => steps,
        Err(err) => {
            eprintln!("MUX_SCRIPT: {err}");
            std::process::exit(1);
        }
    };
    let out = std::env::var_os("MUX_SCRIPT_OUT")
        .map_or_else(|| PathBuf::from("target/shots"), PathBuf::from);
    ctx.enable_accesskit();
    ctx.add_plugin(Script {
        steps: steps.into(),
        frames: VecDeque::new(),
        next: Instant::now() + START_PAUSE,
        widgets: Vec::new(),
        pointer: pos2(0.0, 0.0),
        shot_pending: false,
        out,
        failures: Vec::new(),
        done: false,
    });
}

fn parse(text: &str) -> Result<Vec<(usize, Step)>, String> {
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let at = |what: &str| format!("line {}: {what}", n + 1);
        let step = match verb {
            "wait" => Step::Wait(Duration::from_secs_f64(
                rest.parse().map_err(|_| at("wait takes seconds"))?,
            )),
            "click" => Step::Click(target(rest).ok_or_else(|| at("click what?"))?),
            "hover" => Step::Hover(target(rest).ok_or_else(|| at("hover over what?"))?),
            "type" => Step::Type(rest.to_owned()),
            "key" => {
                let (modifiers, key) =
                    combo(rest).ok_or_else(|| at(&format!("unknown key {rest}")))?;
                Step::Key(modifiers, key)
            }
            "scroll" => Step::Scroll(rest.parse().map_err(|_| at("scroll takes points"))?),
            "expect" if !rest.is_empty() => Step::Expect(rest.to_owned()),
            "shot" if !rest.is_empty() => Step::Shot(rest.to_owned()),
            "labels" => Step::Labels,
            _ => return Err(at(&format!("don't know how to {line}"))),
        };
        steps.push((n + 1, step));
    }
    Ok(steps)
}

fn target(text: &str) -> Option<Target> {
    if text.is_empty() {
        return None;
    }
    let point = text
        .split_once(' ')
        .and_then(|(x, y)| Some(pos2(x.trim().parse().ok()?, y.trim().parse().ok()?)));
    Some(point.map_or_else(|| Target::Label(text.to_owned()), Target::Point))
}

fn combo(text: &str) -> Option<(Modifiers, Key)> {
    let mut modifiers = Modifiers::NONE;
    let mut key = None;
    for part in text.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" => modifiers.alt = true,
            "cmd" => modifiers.command = true,
            name => {
                // egui's own names are capitalised, like Enter and Escape
                let mut chars = name.chars();
                let named = chars
                    .next()
                    .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())?;
                key = Key::from_name(&named).or_else(|| Key::from_name(name));
            }
        }
    }
    if modifiers.ctrl || modifiers.command {
        modifiers.command = true;
        modifiers.ctrl |= !cfg!(target_os = "macos");
        modifiers.mac_cmd = cfg!(target_os = "macos");
    }
    Some((modifiers, key?))
}

impl Script {
    // the widget best matching `label`: an exact label first, then one containing it, smallest first
    fn find(&self, label: &str) -> Option<Rect> {
        let wanted = label.to_lowercase();
        let exact = self
            .widgets
            .iter()
            .filter(|(l, _)| l.to_lowercase() == wanted);
        let near = self
            .widgets
            .iter()
            .filter(|(l, _)| l.to_lowercase().contains(&wanted));
        let smallest = |found: Vec<&(String, Rect)>| {
            found
                .into_iter()
                .min_by(|a, b| a.1.area().total_cmp(&b.1.area()))
                .map(|(_, rect)| *rect)
        };
        smallest(exact.collect()).or_else(|| smallest(near.collect()))
    }

    fn point(&mut self, line: usize, target: &Target) -> Option<Pos2> {
        match target {
            Target::Point(point) => Some(*point),
            Target::Label(label) => {
                let found = self.find(label).map(|rect| rect.center());
                if found.is_none() {
                    self.fail(line, format!("nothing labelled {label:?} is on screen"));
                }
                found
            }
        }
    }

    fn fail(&mut self, line: usize, why: String) {
        eprintln!("MUX_SCRIPT line {line}: {why}");
        self.failures.push(why);
    }

    fn run(&mut self, ctx: &Context, line: usize, step: Step) {
        let mut pause = STEP_PAUSE;
        match step {
            Step::Wait(duration) => pause = duration,
            Step::Click(target) => {
                if let Some(point) = self.point(line, &target) {
                    self.pointer = point;
                    let button = |pressed| Event::PointerButton {
                        pos: point,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    };
                    self.frames.push_back(vec![Event::PointerMoved(point)]);
                    self.frames.push_back(vec![button(true)]);
                    self.frames.push_back(vec![button(false)]);
                }
            }
            Step::Hover(target) => {
                if let Some(point) = self.point(line, &target) {
                    self.pointer = point;
                    self.frames.push_back(vec![Event::PointerMoved(point)]);
                }
            }
            Step::Type(text) => self.frames.push_back(vec![Event::Text(text)]),
            Step::Key(modifiers, key) => {
                let event = |pressed| Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                };
                self.frames.push_back(vec![event(true)]);
                self.frames.push_back(vec![event(false)]);
            }
            Step::Scroll(points) => self.frames.push_back(vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, points),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
                source: egui::MouseWheelSource::Wheel,
            }]),
            Step::Expect(label) => {
                if self.find(&label).is_none() {
                    self.fail(line, format!("expected {label:?} on screen"));
                }
            }
            Step::Labels => {
                for (label, rect) in &self.widgets {
                    eprintln!(
                        "MUX_SCRIPT label {label:?} at {:.0},{:.0}",
                        rect.center().x,
                        rect.center().y
                    );
                }
            }
            Step::Shot(name) => {
                let path = self.out.join(format!("{name}.png"));
                self.shot_pending = true;
                let ctx = ctx.clone();
                // the callback can't reach the plugin, so it says the shot is saved through the context's data
                ctx.clone().request_screenshot(move |image| {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if let Err(err) = crate::capture::save(&path, &image) {
                        eprintln!("MUX_SCRIPT: couldn't save {}: {err}", path.display());
                    }
                    ctx.data_mut(|d| d.insert_temp(shot_id(), true));
                    ctx.request_repaint();
                });
            }
        }
        self.next = Instant::now() + pause;
    }
}

fn shot_id() -> egui::Id {
    egui::Id::unique("mux-script-shot")
}

impl egui::Plugin for Script {
    fn debug_name(&self) -> &'static str {
        "Script"
    }

    fn input_hook(&mut self, ctx: &Context, input: &mut RawInput) {
        // eframe turns the tree off whenever no screen reader is listening, and the script reads it every frame
        ctx.enable_accesskit();
        if let Some(events) = self.frames.pop_front() {
            input.events.extend(events);
            return;
        }
        if self.shot_pending {
            if ctx.data_mut(|d| d.remove_temp::<bool>(shot_id())).is_some() {
                self.shot_pending = false;
            } else {
                return;
            }
        }
        if self.done || Instant::now() < self.next {
            return;
        }
        match self.steps.pop_front() {
            Some((line, step)) => self.run(ctx, line, step),
            None => {
                self.done = true;
                let code = i32::from(!self.failures.is_empty());
                eprintln!(
                    "MUX_SCRIPT: done, {}",
                    if code == 0 {
                        "every step passed".to_owned()
                    } else {
                        format!("{} failed", self.failures.len())
                    }
                );
                // straight out, since closing the window would report success whatever happened
                std::process::exit(code);
            }
        }
    }

    fn output_hook(&mut self, ctx: &Context, output: &mut FullOutput) {
        if let Some(update) = &output.platform_output.accesskit_update {
            self.widgets = update
                .nodes
                .iter()
                .filter(|(_, node)| !node.is_disabled())
                .filter_map(|(_, node)| {
                    let label = node.label().or(node.value())?.trim();
                    let b = node.bounds()?;
                    let rect = Rect::from_min_max(
                        pos2(b.x0 as f32, b.y0 as f32),
                        pos2(b.x1 as f32, b.y1 as f32),
                    );
                    (!label.is_empty() && rect.area() > 0.0).then(|| (label.to_owned(), rect))
                })
                .collect();
        }
        // keeps frames coming while there's a script to run, since nothing else may be asking for them
        if !self.done {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_read_line_by_line() {
        let steps = parse("# open settings\nwait 0.5\nclick Settings\nclick 10 20\nkey ctrl+k\ntype hi there\nshot one\n").unwrap();
        let steps: Vec<Step> = steps.into_iter().map(|(_, s)| s).collect();
        assert_eq!(steps[0], Step::Wait(Duration::from_millis(500)));
        assert_eq!(steps[1], Step::Click(Target::Label("Settings".into())));
        assert_eq!(steps[2], Step::Click(Target::Point(pos2(10.0, 20.0))));
        assert!(matches!(steps[3], Step::Key(m, Key::K) if m.command));
        assert_eq!(steps[4], Step::Type("hi there".into()));
        assert_eq!(steps[5], Step::Shot("one".into()));
        assert!(parse("dance").unwrap_err().contains("line 1"));
        assert!(parse("key nonsense").is_err());
        assert_eq!(combo("Enter").map(|(_, k)| k), Some(Key::Enter));
        assert_eq!(combo("escape").map(|(_, k)| k), Some(Key::Escape));
    }
}
