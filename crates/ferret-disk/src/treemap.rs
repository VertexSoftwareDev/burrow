//! The treemap: every file a rectangle whose area is its size.
//!
//! # Layout
//!
//! Squarified (Bruls, Huizing & van Wijk, 2000): children are placed in rows
//! along the shorter side of the space left, and a row takes another child
//! only while that makes its worst aspect ratio better. The result is tiles
//! that are close to square, which is what makes sizes comparable by eye.
//!
//! A system disk has over a million files and a screen has under a million
//! pixels, so most files cannot be drawn. Children are visited largest first
//! and the tail that would come out smaller than [`MIN_AREA`] is gathered into
//! one "small items" tile — it keeps its true share of the area, it just is
//! not subdivided. That bounds the tile count by the pixel count, not the
//! file count.
//!
//! # Drawing
//!
//! Every tile goes into one mesh, built once per layout and reused every
//! frame until the view changes; a frame costs one draw call plus a few
//! hundred labels. Files get a cushion: lighter top-left, darker
//! bottom-right, so neighbouring tiles of the same colour still read as
//! separate things.

use std::sync::Arc;

use eframe::egui::{self, pos2, vec2, Color32, Mesh, Pos2, Rect};
use ferret_core::Index;
use ferret_tree::{Kind, NodeId, Tree};

use crate::prefs::Metric;
use crate::theme::{self, Theme};

/// Below this many square points a tile is not drawn on its own.
const MIN_AREA: f32 = 14.0;
/// A folder needs at least this much room to show its name above its content.
const HEADER: f32 = 16.0;
const HEADER_MIN_W: f32 = 64.0;
const HEADER_MIN_H: f32 = 44.0;

/// What a tile stands for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum What {
    /// A folder; its children are drawn inside it.
    Dir {
        depth: u16,
        header: bool,
    },
    File(Kind),
    /// The many small children of `node`, gathered into one tile.
    Rest {
        count: u32,
        bytes: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub rect: Rect,
    /// The file or folder; for [`What::Rest`], the folder they belong to.
    pub node: NodeId,
    pub what: What,
}

/// Everything that decides a layout. When none of it changes, neither does
/// the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    pub generation: u64,
    pub focus: NodeId,
    pub rect: Rect,
    pub metric: Metric,
    pub theme: Theme,
}

pub struct Layout {
    pub key: Key,
    /// In drawing order: a folder before what it contains.
    pub tiles: Vec<Tile>,
    mesh: Arc<Mesh>,
}

fn value(tree: &Tree, node: NodeId, metric: Metric) -> u64 {
    let totals = tree.totals(node);
    match metric {
        Metric::OnDisk => totals.allocated,
        Metric::Size => totals.size,
    }
}

impl Layout {
    pub fn build(index: &Index, tree: &Tree, key: Key) -> Layout {
        let mut tiles = Vec::new();
        let mut builder = Builder {
            index,
            tree,
            metric: key.metric,
            tiles: &mut tiles,
            scratch: Vec::new(),
        };
        builder.place(key.focus, key.rect, 0);

        let mesh = build_mesh(&tiles, key.theme);
        Layout {
            key,
            tiles,
            mesh: Arc::new(mesh),
        }
    }

    /// The deepest tile under `pos`.
    pub fn hit(&self, pos: Pos2) -> Option<&Tile> {
        self.tiles.iter().rev().find(|t| t.rect.contains(pos))
    }

    /// Where `node` is drawn, if it is.
    pub fn rect_of(&self, node: NodeId) -> Option<Rect> {
        self.tiles
            .iter()
            .find(|t| t.node == node && !matches!(t.what, What::Rest { .. }))
            .map(|t| t.rect)
    }

    pub fn paint(&self, painter: &egui::Painter, paint: &Paint) {
        painter.add(egui::Shape::mesh(self.mesh.clone()));

        let p = theme::palette(paint.theme);
        let small = egui::FontId::proportional(11.0);
        let label = egui::FontId::proportional(12.0);

        for tile in &self.tiles {
            match tile.what {
                What::Dir { header: true, .. } => {
                    let strip = Rect::from_min_size(tile.rect.min, vec2(tile.rect.width(), HEADER));
                    let text = format!(
                        "{}  {}",
                        paint.tree.name(paint.index, tile.node),
                        crate::format::size(paint.lang, value(paint.tree, tile.node, paint.metric))
                    );
                    painter.with_clip_rect(strip.shrink(1.0)).text(
                        strip.left_center() + vec2(5.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        text,
                        small.clone(),
                        p.text,
                    );
                }
                What::File(kind) if tile.rect.width() >= 64.0 && tile.rect.height() >= 30.0 => {
                    let ink = ink_on(theme::kind_colour(paint.theme, kind));
                    let clip = painter.with_clip_rect(tile.rect.shrink(3.0));
                    let top = tile.rect.left_top() + vec2(5.0, 4.0);
                    clip.text(
                        top,
                        egui::Align2::LEFT_TOP,
                        paint.index.name(tile.node as usize),
                        label.clone(),
                        ink,
                    );
                    clip.text(
                        top + vec2(0.0, 15.0),
                        egui::Align2::LEFT_TOP,
                        crate::format::size(paint.lang, value(paint.tree, tile.node, paint.metric)),
                        small.clone(),
                        ink.gamma_multiply(0.8),
                    );
                }
                What::Rest { count, .. }
                    if tile.rect.width() >= 70.0 && tile.rect.height() >= 18.0 =>
                {
                    painter.with_clip_rect(tile.rect.shrink(2.0)).text(
                        tile.rect.center(),
                        egui::Align2::CENTER_CENTER,
                        paint.lang.small_items(count as usize),
                        small.clone(),
                        p.muted,
                    );
                }
                _ => {}
            }
        }

        let outline = |node: NodeId, stroke: egui::Stroke| {
            if let Some(rect) = self.rect_of(node) {
                painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
            }
        };
        if let Some(node) = paint.hovered {
            outline(node, egui::Stroke::new(1.0, p.text));
        }
        if let Some(node) = paint.selected {
            outline(node, egui::Stroke::new(2.0, p.accent));
        }
    }
}

/// What painting needs besides the layout.
pub struct Paint<'a> {
    pub index: &'a Index,
    pub tree: &'a Tree,
    pub theme: Theme,
    pub lang: crate::i18n::Lang,
    pub metric: Metric,
    pub selected: Option<NodeId>,
    pub hovered: Option<NodeId>,
}

struct Builder<'a> {
    index: &'a Index,
    tree: &'a Tree,
    metric: Metric,
    tiles: &'a mut Vec<Tile>,
    /// Reused per folder, so a deep tree does not allocate per level.
    scratch: Vec<Rect>,
}

impl Builder<'_> {
    /// Lay out the children of `node` inside `rect`.
    fn place(&mut self, node: NodeId, rect: Rect, depth: u16) {
        let area = rect.area();
        if area < MIN_AREA || !rect.is_positive() {
            return;
        }

        let mut children: Vec<(NodeId, u64)> = self
            .tree
            .children(node)
            .iter()
            .map(|c| (*c, value(self.tree, *c, self.metric)))
            .filter(|(_, v)| *v > 0)
            .collect();
        if children.is_empty() {
            return;
        }
        // Already largest-first by size on disk; logical size can differ.
        if self.metric == Metric::Size {
            children.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        }

        let total: u64 = children.iter().map(|(_, v)| v).sum();
        let scale = area as f64 / total as f64;

        // Keep children while they would be big enough to see; the rest
        // become one tile with their combined share.
        let mut kept = children.len();
        for (i, (_, v)) in children.iter().enumerate() {
            if (*v as f64 * scale) < MIN_AREA as f64 {
                kept = i;
                break;
            }
        }
        let rest: u64 = children[kept..].iter().map(|(_, v)| v).sum();

        let mut areas: Vec<f64> = children[..kept]
            .iter()
            .map(|(_, v)| *v as f64 * scale)
            .collect();
        if rest > 0 {
            areas.push(rest as f64 * scale);
        }
        // Squarify wants largest first; the rest tile may be larger than the
        // last kept child, so order it by value too.
        let mut order: Vec<usize> = (0..areas.len()).collect();
        order.sort_by(|a, b| areas[*b].total_cmp(&areas[*a]));
        let sorted: Vec<f64> = order.iter().map(|i| areas[*i]).collect();

        let mut rects = std::mem::take(&mut self.scratch);
        rects.clear();
        squarify(&sorted, rect, &mut rects);
        let mut placed = vec![Rect::NOTHING; areas.len()];
        for (slot, r) in order.iter().zip(rects.iter()) {
            placed[*slot] = *r;
        }
        self.scratch = rects;

        if rest > 0 {
            let r = placed[kept];
            if r.is_positive() {
                self.tiles.push(Tile {
                    rect: r,
                    node,
                    what: What::Rest {
                        count: (children.len() - kept) as u32,
                        bytes: rest,
                    },
                });
            }
        }

        for (i, (child, _)) in children[..kept].iter().enumerate() {
            let r = placed[i];
            if !r.is_positive() {
                continue;
            }
            if self.tree.is_dir(*child) {
                let header = r.width() >= HEADER_MIN_W && r.height() >= HEADER_MIN_H;
                self.tiles.push(Tile {
                    rect: r,
                    node: *child,
                    what: What::Dir {
                        depth: depth + 1,
                        header,
                    },
                });
                let pad = if r.width() > 12.0 && r.height() > 12.0 {
                    2.0
                } else {
                    0.5
                };
                let mut inner = r.shrink(pad);
                if header {
                    inner.min.y = r.min.y + HEADER;
                }
                self.place(*child, inner, depth + 1);
            } else {
                let kind = Kind::of(self.index.name(*child as usize));
                self.tiles.push(Tile {
                    rect: r,
                    node: *child,
                    what: What::File(kind),
                });
            }
        }
    }
}

/// Squarified layout of `areas` (largest first, summing to the area of
/// `rect`) into `out`, one rectangle per area, in the same order.
pub fn squarify(areas: &[f64], rect: Rect, out: &mut Vec<Rect>) {
    let mut rest = rect;
    let mut i = 0;
    let n = areas.len();

    while i < n {
        let short = rest.width().min(rest.height()) as f64;
        if short <= 0.0 {
            out.extend(std::iter::repeat_n(Rect::NOTHING, n - i));
            return;
        }

        // Grow the row while its worst aspect ratio improves.
        let mut end = i + 1;
        let mut sum = areas[i];
        let mut worst = worst_ratio(areas[i], areas[i], sum, short);
        while end < n {
            let grown = sum + areas[end];
            let ratio = worst_ratio(areas[i], areas[end], grown, short);
            if ratio > worst {
                break;
            }
            worst = ratio;
            sum = grown;
            end += 1;
        }

        let thickness = (sum / short) as f32;
        if rest.width() >= rest.height() {
            // A column down the left edge.
            let thickness = thickness.min(rest.width());
            let mut y = rest.min.y;
            for a in &areas[i..end] {
                let h = (*a / sum) as f32 * rest.height();
                out.push(Rect::from_min_size(pos2(rest.min.x, y), vec2(thickness, h)));
                y += h;
            }
            rest.min.x += thickness;
        } else {
            // A row along the top edge.
            let thickness = thickness.min(rest.height());
            let mut x = rest.min.x;
            for a in &areas[i..end] {
                let w = (*a / sum) as f32 * rest.width();
                out.push(Rect::from_min_size(pos2(x, rest.min.y), vec2(w, thickness)));
                x += w;
            }
            rest.min.y += thickness;
        }
        i = end;
    }
}

/// The worse of the two extreme aspect ratios in a row of total `sum` laid
/// along a side of length `side`, whose largest and smallest members are
/// `max` and `min`.
fn worst_ratio(max: f64, min: f64, sum: f64, side: f64) -> f64 {
    let s2 = sum * sum;
    let w2 = side * side;
    (w2 * max / s2).max(s2 / (w2 * min))
}

fn build_mesh(tiles: &[Tile], theme: Theme) -> Mesh {
    let p = theme::palette(theme);
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(tiles.len() * 4);
    mesh.reserve_triangles(tiles.len() * 2);

    for tile in tiles {
        match tile.what {
            What::Dir { depth, .. } => {
                // Nested folders alternate between two quiet shades, so a
                // folder's edge is visible without a stroke per tile.
                let t = if depth % 2 == 0 { 0.30 } else { 0.55 };
                mesh.add_colored_rect(tile.rect, mix(p.panel, p.border, t));
            }
            What::File(kind) => {
                let base = theme::kind_colour(theme, kind);
                cushion(&mut mesh, gap(tile.rect), base);
            }
            What::Rest { .. } => {
                mesh.add_colored_rect(gap(tile.rect), mix(p.panel, p.border, 0.85));
            }
        }
    }
    mesh
}

/// Leave a hairline of the folder showing between neighbours.
fn gap(rect: Rect) -> Rect {
    if rect.width() > 4.0 && rect.height() > 4.0 {
        rect.shrink(0.5)
    } else {
        rect
    }
}

/// A rectangle lit from the top left.
fn cushion(mesh: &mut Mesh, rect: Rect, base: Color32) {
    let first = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), mix(base, Color32::WHITE, 0.22));
    mesh.colored_vertex(rect.right_top(), mix(base, Color32::WHITE, 0.06));
    mesh.colored_vertex(rect.right_bottom(), mix(base, Color32::BLACK, 0.22));
    mesh.colored_vertex(rect.left_bottom(), mix(base, Color32::BLACK, 0.04));
    mesh.add_triangle(first, first + 1, first + 2);
    mesh.add_triangle(first, first + 2, first + 3);
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Near-black or white, whichever stands off a tile colour.
fn ink_on(fill: Color32) -> Color32 {
    let [r, g, b, _] = fill.to_array().map(|c| c as f32 / 255.0);
    if 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.55 {
        Color32::from_rgb(0x0b, 0x0b, 0x0b)
    } else {
        Color32::WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferret_core::testing::{dir, file, index_from_specs};
    use ferret_core::ROOT_RECORD;

    fn total_area(rects: &[Rect]) -> f32 {
        rects.iter().map(|r| r.area()).sum()
    }

    #[test]
    fn squarify_fills_the_rect_exactly_once() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(600.0, 400.0));
        let areas = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0].map(|a| a * 240_000.0 / 24.0);
        let mut out = Vec::new();
        squarify(&areas, rect, &mut out);

        assert_eq!(out.len(), areas.len());
        assert!((total_area(&out) - rect.area()).abs() < 1.0);
        for (r, a) in out.iter().zip(areas) {
            assert!(
                (r.area() as f64 - a).abs() < 1.0,
                "{r:?} should have area {a}"
            );
            assert!(rect.expand(0.01).contains_rect(*r));
        }
        // No two tiles overlap.
        for (i, a) in out.iter().enumerate() {
            for b in &out[i + 1..] {
                assert!(a.intersect(*b).area() < 0.01, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn squarify_keeps_tiles_close_to_square() {
        // The textbook example: a 6x4 rectangle with these areas.
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(6.0, 4.0));
        let mut out = Vec::new();
        squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], rect, &mut out);
        let worst = out
            .iter()
            .map(|r| (r.width() / r.height()).max(r.height() / r.width()))
            .fold(0.0f32, f32::max);
        assert!(worst < 3.0, "worst aspect ratio {worst}");
    }

    fn key(rect: Rect, focus: NodeId) -> Key {
        Key {
            generation: 1,
            focus,
            rect,
            metric: Metric::OnDisk,
            theme: Theme::Dark,
        }
    }

    #[test]
    fn a_layout_nests_files_inside_their_folders() {
        let index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            file(21, 20, "movie.mkv").sized(8 << 30),
            file(22, ROOT_RECORD, "pagefile.sys").sized(4 << 30),
        ]);
        let tree = Tree::build(&index);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let layout = Layout::build(&index, &tree, key(rect, tree.root()));

        let users = layout.rect_of(0).expect("Users is drawn");
        let movie = layout.rect_of(1).expect("the movie is drawn");
        assert!(users.contains_rect(movie));
        // Two thirds of the disk, two thirds of the map (less the padding).
        assert!((users.area() / rect.area() - 2.0 / 3.0).abs() < 0.02);

        // The deepest tile wins a hit test.
        assert_eq!(layout.hit(movie.center()).map(|t| t.node), Some(1));
    }

    #[test]
    fn a_crowd_of_tiny_files_becomes_one_tile() {
        let mut specs = vec![file(20, ROOT_RECORD, "big.bin").sized(1 << 30)];
        let names: Vec<&'static str> = (0..5000)
            .map(|i| &*Box::leak(format!("f{i}.txt").into_boxed_str()))
            .collect();
        for (i, name) in names.iter().enumerate() {
            specs.push(file(21 + i as u32, ROOT_RECORD, name).sized(4096));
        }
        let index = index_from_specs(specs);
        let tree = Tree::build(&index);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let layout = Layout::build(&index, &tree, key(rect, tree.root()));

        let rest: Vec<_> = layout
            .tiles
            .iter()
            .filter(|t| matches!(t.what, What::Rest { .. }))
            .collect();
        assert_eq!(rest.len(), 1);
        assert!(matches!(rest[0].what, What::Rest { count: 5000, .. }));
        assert!(layout.tiles.len() < 10);
    }

    #[test]
    fn an_empty_folder_draws_nothing() {
        let index = index_from_specs(vec![dir(20, ROOT_RECORD, "empty")]);
        let tree = Tree::build(&index);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        assert!(Layout::build(&index, &tree, key(rect, tree.root()))
            .tiles
            .is_empty());
    }
}
