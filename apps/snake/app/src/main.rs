#![cfg_attr(feature = "target_vanadium_ledger", no_std, no_main)]

extern crate alloc;

mod game;

use alloc::format;
use sdk::executor::block_on;
use sdk::ui::{Align, Color, ContentHint, Font, InputModel, Rect, Renderer, ScreenRenderer};
use sdk::ux::{ButtonEvent, Event};

use game::{Cell, Game};

sdk::bootstrap!();

/// Side of a grid cell, in pixels. The Nano S+ screen is 128×64, giving a 32×16 board.
const CELL: i32 = 4;
/// Tickers between snake moves (≈10 tickers/s, so ≈5 moves/s).
const MOVE_TICKS: u32 = 2;
/// Idle tickers to keep the "game over" screen up before returning (≈3 s).
const GAME_OVER_TICKS: u32 = 30;

/// The two menu entries, in display order.
const MENU_ITEMS: [&str; 2] = ["Start", "Exit"];

/// Game board colors: a white snake (and food) on a black background.
const BOARD_BG: Color = Color::Black;
const BOARD_FG: Color = Color::White;

// Pixel rectangle of a whole grid cell.
fn cell_rect(c: Cell) -> Rect {
    Rect::new(c.0 as i32 * CELL, c.1 as i32 * CELL, CELL, CELL)
}

// Pixel rectangle of the food dot: a 2×2 square centered in its cell, so it reads as a
// pellet distinct from the solid snake body (the monochrome panel has no color to spare).
fn food_rect(c: Cell) -> Rect {
    Rect::new(c.0 as i32 * CELL + 1, c.1 as i32 * CELL + 1, 2, 2)
}

fn draw_cell(r: &mut ScreenRenderer, c: Cell, color: Color) {
    r.fill_rect(cell_rect(c), color);
}

// Centered two-line message (used for "game over" and the unsupported-device notice).
fn paint_message(r: &mut ScreenRenderer, w: i32, h: i32, line1: &str, line2: &str) {
    let lh = r.caps().font(Font::Bold).line_height as i32;
    r.fill_rect(Rect::new(0, 0, w, h), Color::White);
    let total = lh * 2;
    let y = ((h - total) / 2).max(0);
    r.text(Rect::new(0, y, w, lh), line1, Font::Bold, Color::Black, Color::White, Align::Center);
    r.text(Rect::new(0, y + lh, w, lh), line2, Font::Regular, Color::Black, Color::White, Align::Center);
    r.present(ContentHint::FullScreen);
}

// -----------------------------------------------------------------------------
// Menu
// -----------------------------------------------------------------------------

// Repaints the whole menu, highlighting the selected entry (white-on-black, since the
// monochrome panel has no color to spare for a subtler highlight).
fn paint_menu(r: &mut ScreenRenderer, w: i32, h: i32, selected: usize) {
    let lh = r.caps().font(Font::Bold).line_height as i32;
    r.fill_rect(Rect::new(0, 0, w, h), Color::White);

    // Title pinned to the top.
    r.text(Rect::new(0, 2, w, lh), "SNAKE", Font::Bold, Color::Black, Color::White, Align::Center);

    // The two entries stacked and centered in the area below the title.
    let gap = 4;
    let block = MENU_ITEMS.len() as i32 * lh + (MENU_ITEMS.len() as i32 - 1) * gap;
    let mut y = (((h + lh) - block) / 2).max(lh + 4);
    for (i, item) in MENU_ITEMS.iter().enumerate() {
        let row = Rect::new(8, y, w - 16, lh);
        if i == selected {
            r.fill_rect(row, Color::Black);
            r.text(row, item, Font::Bold, Color::White, Color::Black, Align::Center);
        } else {
            r.text(row, item, Font::Bold, Color::Black, Color::White, Align::Center);
        }
        y += lh + gap;
    }
    r.present(ContentHint::FullScreen);
}

/// Runs the menu until the user selects an entry, returning its index. Navigation uses
/// button **releases** (the Ledger convention, so a both-buttons select resolves cleanly):
/// left/right cycle the highlight, both select. `seed` is advanced on every ticker so the
/// time spent here perturbs the game's food placement.
async fn menu(r: &mut ScreenRenderer, w: i32, h: i32, seed: &mut u32) -> usize {
    let n = MENU_ITEMS.len();
    let mut selected = 0usize;
    paint_menu(r, w, h, selected);

    loop {
        match sdk::ux::get_event().await {
            Event::Button(ButtonEvent::LeftRelease) => {
                selected = (selected + n - 1) % n;
                paint_menu(r, w, h, selected);
            }
            Event::Button(ButtonEvent::RightRelease) => {
                selected = (selected + 1) % n;
                paint_menu(r, w, h, selected);
            }
            Event::Button(ButtonEvent::BothRelease) => return selected,
            Event::Ticker => *seed = seed.wrapping_add(1),
            _ => {}
        }
    }
}

// -----------------------------------------------------------------------------
// Game round
// -----------------------------------------------------------------------------

// Paints the whole board from scratch and refreshes the full panel.
fn paint_board(r: &mut ScreenRenderer, g: &Game, w: i32, h: i32) {
    r.fill_rect(Rect::new(0, 0, w, h), BOARD_BG);
    for &c in g.body() {
        draw_cell(r, c, BOARD_FG);
    }
    r.fill_rect(food_rect(g.food()), BOARD_FG);
    r.present(ContentHint::FullScreen);
}

/// Plays one round to its end, then (unless the player quit with both buttons) shows the
/// final score until a button is released or a short timeout elapses.
async fn play_round(r: &mut ScreenRenderer, w: i32, h: i32, seed: u32) {
    let cols = (w / CELL) as u8;
    let rows = (h / CELL) as u8;
    let mut g = Game::new(cols, rows, seed);

    paint_board(r, &g, w, h);

    // At most one turn is applied per move, so a quick double-tap can't fold the snake back
    // on itself (two left turns = a reversal). Left/right are handled on *press* for an
    // instant response, as a game needs — unlike the menu, which acts on release.
    let mut turned = false;
    let mut ticks = 0u32;
    let mut quit = false;
    loop {
        match sdk::ux::get_event().await {
            Event::Button(ButtonEvent::LeftPress) if !turned => {
                g.turn_left();
                turned = true;
            }
            Event::Button(ButtonEvent::RightPress) if !turned => {
                g.turn_right();
                turned = true;
            }
            // Pressing both buttons abandons the round and returns to the menu.
            Event::Button(ButtonEvent::BothPress) => {
                quit = true;
                break;
            }
            Event::Ticker => {
                ticks += 1;
                if ticks < MOVE_TICKS {
                    continue;
                }
                ticks = 0;
                turned = false;

                let res = g.step();
                if res.over {
                    break;
                }
                if let Some(tail) = res.removed_tail {
                    draw_cell(r, tail, BOARD_BG);
                }
                if let Some(head) = res.added_head {
                    draw_cell(r, head, BOARD_FG);
                }
                if let Some(food) = res.new_food {
                    r.fill_rect(food_rect(food), BOARD_FG);
                }
                r.present(ContentHint::Graphics);
            }
            _ => {}
        }
    }

    if quit {
        return;
    }

    let title = if g.won() { "You win!" } else { "Game over" };
    paint_message(r, w, h, title, &format!("Score: {}", g.score()));

    // Hold the result on screen; a button release dismisses it sooner.
    let mut idle = 0u32;
    loop {
        match sdk::ux::get_event().await {
            Event::Button(ButtonEvent::LeftRelease)
            | Event::Button(ButtonEvent::RightRelease)
            | Event::Button(ButtonEvent::BothRelease) => break,
            Event::Ticker => {
                idle += 1;
                if idle >= GAME_OVER_TICKS {
                    break;
                }
            }
            _ => {}
        }
    }
}

// -----------------------------------------------------------------------------
// Entry point
// -----------------------------------------------------------------------------

async fn run() {
    let mut r = ScreenRenderer::new();
    let (w, h) = (r.caps().size.w as i32, r.caps().size.h as i32);

    // This game is driven by the two physical buttons of the Nano devices; on a touch
    // device there is nothing to navigate it with.
    if r.caps().input != InputModel::TwoButton {
        paint_message(&mut r, w, h, "Snake", "Nano S+ only");
        sdk::ux::wait(GAME_OVER_TICKS).await;
        sdk::exit(0);
    }

    let mut seed: u32 = 0x1234_5678;
    loop {
        match menu(&mut r, w, h, &mut seed).await {
            0 => play_round(&mut r, w, h, seed).await,
            _ => sdk::exit(0),
        }
    }
}

pub fn main() {
    block_on(run());
}
