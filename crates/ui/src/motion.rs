use std::f32::consts::TAU;

use egui::{Context, Id};

// parameters match swiftui so values can be copied from apple's docs and wwdc talks
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub response: f32,
    pub damping: f32,
}

impl Spring {
    pub const QUICK: Self = Self {
        response: 0.18,
        damping: 1.0,
    };
    pub const SMOOTH: Self = Self {
        response: 0.35,
        damping: 1.0,
    };
    pub const SNAPPY: Self = Self {
        response: 0.3,
        damping: 0.8,
    };
    pub const BOUNCY: Self = Self {
        response: 0.45,
        damping: 0.65,
    };

    pub fn new(response: f32, damping: f32) -> Self {
        Self { response, damping }
    }
}

#[derive(Clone, Copy, Debug)]
struct State {
    value: f32,
    velocity: f32,
}

fn reduced_id() -> Id {
    Id::unique("mux-reduced-motion")
}

pub fn set_reduced(ctx: &Context, reduced: bool) {
    ctx.data_mut(|d| d.insert_temp(reduced_id(), reduced));
}

pub fn reduced(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp(reduced_id())).unwrap_or(false)
}

const STEP: f32 = 1.0 / 240.0;
const REST: f32 = 0.0005;

pub fn spring(ctx: &Context, id: Id, target: f32, spring: Spring) -> f32 {
    if reduced(ctx) {
        ctx.data_mut(|d| {
            d.insert_temp(
                id,
                State {
                    value: target,
                    velocity: 0.0,
                },
            )
        });
        return target;
    }
    let dt = ctx.input(|i| i.stable_dt).min(1.0 / 20.0);
    let mut s = ctx.data_mut(|d| {
        *d.get_temp_mut_or_insert_with(id, || State {
            value: target,
            velocity: 0.0,
        })
    });

    let k = (TAU / spring.response).powi(2);
    let c = 4.0 * std::f32::consts::PI * spring.damping / spring.response;
    // fixed substeps keep stiff springs stable when a frame takes long
    let steps = (dt / STEP).ceil().max(1.0) as usize;
    let h = dt / steps as f32;
    for _ in 0..steps {
        let accel = -k * (s.value - target) - c * s.velocity;
        s.velocity += accel * h;
        s.value += s.velocity * h;
    }

    if (s.value - target).abs() < REST && s.velocity.abs() < REST {
        s = State {
            value: target,
            velocity: 0.0,
        };
    } else {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, s));
    s.value
}

pub fn spring_bool(ctx: &Context, id: Id, on: bool, spring: Spring) -> f32 {
    self::spring(ctx, id, if on { 1.0 } else { 0.0 }, spring)
}
