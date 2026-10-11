//! rasterization: vertices in clip space through the viewport into window
//! coordinates, and the pixels each triangle, line or point covers, with
//! its varyings interpolated for each, perspective-correct. what happens
//! to a fragment, its shading, its tests and its blending, is the caller's.

use crate::state::Viewport;

/// a vertex as the vertex program leaves it: its position in clip space
/// and its varyings, four floats each.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipVertex {
    pub position: [f32; 4],
    pub varyings: Vec<[f32; 4]>,
    /// the point size the program wrote, if it did.
    pub point_size: Option<f32>,
}

/// a vertex in window coordinates, its varyings divided by w for
/// perspective-correct interpolation, with 1/w kept.
#[derive(Debug, Clone)]
struct WindowVertex {
    x: f32,
    y: f32,
    z: f32,
    inverse_w: f32,
    varyings: Vec<[f32; 4]>,
}

/// a pixel a primitive covers.
#[derive(Debug, Clone)]
pub struct Fragment<'a> {
    pub x: u32,
    pub y: u32,
    /// window depth, after the viewport.
    pub z: f32,
    /// clip space w at the pixel.
    pub w: f32,
    pub front_facing: bool,
    pub varyings: &'a [[f32; 4]],
}

/// the rectangle fragments may land in, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub x_min: u32,
    pub y_min: u32,
    pub x_max: u32,
    pub y_max: u32,
}

/// clip space w below which a vertex is behind the eye.
const NEAR_W: f32 = 1e-5;

fn to_window(vertex: &ClipVertex, viewport: &Viewport) -> WindowVertex {
    let [x, y, z, w] = vertex.position;
    let (x, y, z, inverse_w) = if viewport.enabled {
        let inverse_w = 1.0 / w;
        (
            x * inverse_w * viewport.scale[0] + viewport.offset[0],
            y * inverse_w * viewport.scale[1] + viewport.offset[1],
            z * inverse_w * viewport.scale[2] + viewport.offset[2],
            inverse_w,
        )
    } else {
        // the program gives window coordinates itself, w included
        let inverse_w = if w != 0.0 { 1.0 / w } else { 1.0 };
        (x * inverse_w, y * inverse_w, z * inverse_w, inverse_w)
    };
    let varyings = vertex.varyings.iter().map(|varying| varying.map(|component| component * inverse_w)).collect();
    WindowVertex { x, y, z, inverse_w, varyings }
}

/// cuts a polygon against w = NEAR_W, keeping the part in front of the eye.
fn clip_near(polygon: &[ClipVertex]) -> Vec<ClipVertex> {
    let mut out = Vec::with_capacity(polygon.len() + 2);
    for (index, current) in polygon.iter().enumerate() {
        let next = &polygon[(index + 1) % polygon.len()];
        let (current_in, next_in) = (current.position[3] >= NEAR_W, next.position[3] >= NEAR_W);
        if current_in {
            out.push(current.clone());
        }
        if current_in != next_in {
            let t = (NEAR_W - current.position[3]) / (next.position[3] - current.position[3]);
            out.push(lerp_vertex(current, next, t));
        }
    }
    out
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|index| lerp(a[index], b[index], t))
}

fn lerp_vertex(a: &ClipVertex, b: &ClipVertex, t: f32) -> ClipVertex {
    ClipVertex {
        position: lerp4(a.position, b.position, t),
        varyings: a.varyings.iter().zip(&b.varyings).map(|(&a, &b)| lerp4(a, b, t)).collect(),
        point_size: a.point_size,
    }
}

/// twice the signed area of a triangle in window coordinates, positive
/// when its vertices go counter-clockwise with y growing downwards.
fn area(a: &WindowVertex, b: &WindowVertex, c: &WindowVertex) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)
}

/// whether a triangle in clip space faces the viewer, before clipping, as
/// the window would show it: counter-clockwise on screen is the front.
pub fn is_front_facing(vertices: [&ClipVertex; 3], viewport: &Viewport) -> bool {
    let window = vertices.map(|vertex| to_window(vertex, viewport));
    let mut signed = area(&window[0], &window[1], &window[2]);
    // a vertex behind the eye flips its projection
    let behind = vertices.iter().filter(|vertex| vertex.position[3] < 0.0).count();
    if behind % 2 == 1 {
        signed = -signed;
    }
    signed < 0.0
}

/// the pixels a triangle covers, each handed to fragment with its
/// interpolated varyings.
pub fn triangle(
    vertices: [&ClipVertex; 3],
    viewport: &Viewport,
    bounds: Bounds,
    front_facing: bool,
    fragment: &mut impl FnMut(&Fragment),
) {
    let clipped = clip_near(&[vertices[0].clone(), vertices[1].clone(), vertices[2].clone()]);
    if clipped.len() < 3 {
        return;
    }
    let window: Vec<WindowVertex> = clipped.iter().map(|vertex| to_window(vertex, viewport)).collect();
    for index in 1..window.len() - 1 {
        fill(&window[0], &window[index], &window[index + 1], bounds, front_facing, fragment);
    }
}

/// whether an edge is a top or a left one, whose pixels exactly on it
/// belong to the triangle, for a triangle whose area is positive.
fn top_left(from: &WindowVertex, to: &WindowVertex) -> bool {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    (dy == 0.0 && dx < 0.0) || dy > 0.0
}

fn fill(
    a: &WindowVertex,
    b: &WindowVertex,
    c: &WindowVertex,
    bounds: Bounds,
    front_facing: bool,
    fragment: &mut impl FnMut(&Fragment),
) {
    // with the vertices turned to give a positive area, the edge functions
    // are positive inside
    let (a, b, c) = if area(a, b, c) < 0.0 { (a, c, b) } else { (a, b, c) };
    let total = area(a, b, c);
    if total <= 0.0 || !total.is_finite() {
        return;
    }
    let x_min = a.x.min(b.x).min(c.x).floor().max(bounds.x_min as f32);
    let y_min = a.y.min(b.y).min(c.y).floor().max(bounds.y_min as f32);
    let x_max = a.x.max(b.x).max(c.x).ceil().min(bounds.x_max as f32);
    let y_max = a.y.max(b.y).max(c.y).ceil().min(bounds.y_max as f32);
    if x_min > x_max || y_min > y_max {
        return;
    }
    let edges = [(b, c), (c, a), (a, b)];
    let owned = edges.map(|(from, to)| top_left(from, to));
    let count = a.varyings.len();
    let mut varyings = vec![[0.0f32; 4]; count];
    for y in y_min as u32..=y_max as u32 {
        let py = y as f32 + 0.5;
        for x in x_min as u32..=x_max as u32 {
            let px = x as f32 + 0.5;
            let weights = edges.map(|(from, to)| (to.x - from.x) * (py - from.y) - (px - from.x) * (to.y - from.y));
            if weights.iter().zip(&owned).any(|(&weight, &owned)| weight < 0.0 || (weight == 0.0 && !owned)) {
                continue;
            }
            let [l0, l1, l2] = weights.map(|weight| weight / total);
            let inverse_w = l0 * a.inverse_w + l1 * b.inverse_w + l2 * c.inverse_w;
            let w = 1.0 / inverse_w;
            for (index, varying) in varyings.iter_mut().enumerate() {
                let (va, vb, vc) = (a.varyings[index], b.varyings[index], c.varyings[index]);
                *varying =
                    std::array::from_fn(|component| (l0 * va[component] + l1 * vb[component] + l2 * vc[component]) * w);
            }
            let z = l0 * a.z + l1 * b.z + l2 * c.z;
            fragment(&Fragment { x, y, z, w, front_facing, varyings: &varyings });
        }
    }
}

/// the pixels a line covers, width pixels wide, its varyings interpolated
/// along it.
pub fn line(
    vertices: [&ClipVertex; 2],
    viewport: &Viewport,
    bounds: Bounds,
    width: f32,
    fragment: &mut impl FnMut(&Fragment),
) {
    if vertices.iter().any(|vertex| vertex.position[3] < NEAR_W) {
        return;
    }
    let [a, b] = vertices.map(|vertex| to_window(vertex, viewport));
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as u32;
    let half = (width.max(1.0) / 2.0).floor() as i64;
    let mut varyings = vec![[0.0f32; 4]; a.varyings.len()];
    for step in 0..steps {
        let t = (step as f32 + 0.5) / steps as f32;
        let inverse_w = lerp(a.inverse_w, b.inverse_w, t);
        let w = 1.0 / inverse_w;
        for (index, varying) in varyings.iter_mut().enumerate() {
            *varying = lerp4(a.varyings[index], b.varyings[index], t).map(|component| component * w);
        }
        let (x, y, z) = (lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.z, b.z, t));
        let x_major = dx.abs() >= dy.abs();
        for offset in -half..=half.max(0) {
            let (px, py) = if x_major { (x, y + offset as f32) } else { (x + offset as f32, y) };
            emit(px, py, z, w, true, &varyings, bounds, fragment);
        }
    }
}

/// the pixels a point covers, a square size pixels across centred on it.
pub fn point(
    vertex: &ClipVertex,
    viewport: &Viewport,
    bounds: Bounds,
    size: f32,
    fragment: &mut impl FnMut(&Fragment),
) {
    if vertex.position[3] < NEAR_W {
        return;
    }
    let window = to_window(vertex, viewport);
    let w = 1.0 / window.inverse_w;
    let varyings: Vec<[f32; 4]> =
        window.varyings.iter().map(|varying| varying.map(|component| component * w)).collect();
    let half = size.max(1.0) / 2.0;
    let (x0, y0) = ((window.x - half).round(), (window.y - half).round());
    let side = size.max(1.0).round() as u32;
    for dy in 0..side {
        for dx in 0..side {
            emit(x0 + dx as f32 + 0.5, y0 + dy as f32 + 0.5, window.z, w, true, &varyings, bounds, fragment);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit(
    x: f32,
    y: f32,
    z: f32,
    w: f32,
    front_facing: bool,
    varyings: &[[f32; 4]],
    bounds: Bounds,
    fragment: &mut impl FnMut(&Fragment),
) {
    if x < 0.0 || y < 0.0 {
        return;
    }
    let (x, y) = (x as u32, y as u32);
    if x < bounds.x_min || x > bounds.x_max || y < bounds.y_min || y > bounds.y_max {
        return;
    }
    fragment(&Fragment { x, y, z, w, front_facing, varyings });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(x: f32, y: f32, varying: f32) -> ClipVertex {
        ClipVertex { position: [x, y, 0.0, 1.0], varyings: vec![[varying; 4]], point_size: None }
    }

    /// a viewport that leaves window coordinates as they are given.
    fn identity() -> Viewport {
        Viewport { enabled: true, offset: [0.0; 3], scale: [1.0; 3] }
    }

    fn covered(vertices: [&ClipVertex; 3]) -> Vec<(u32, u32, f32)> {
        let bounds = Bounds { x_min: 0, y_min: 0, x_max: 15, y_max: 15 };
        let mut pixels = Vec::new();
        triangle(vertices, &identity(), bounds, true, &mut |fragment| {
            pixels.push((fragment.x, fragment.y, fragment.varyings[0][0]))
        });
        pixels
    }

    #[test]
    fn two_triangles_sharing_an_edge_cover_each_pixel_once() {
        let (a, b, c, d) = (vertex(0.0, 0.0, 0.0), vertex(8.0, 0.0, 0.0), vertex(8.0, 8.0, 0.0), vertex(0.0, 8.0, 0.0));
        let mut pixels: Vec<_> = covered([&a, &b, &c]).into_iter().map(|(x, y, _)| (x, y)).collect();
        pixels.extend(covered([&a, &c, &d]).into_iter().map(|(x, y, _)| (x, y)));
        pixels.sort();
        let count = pixels.len();
        pixels.dedup();
        assert_eq!(count, 64);
        assert_eq!(pixels.len(), 64);
        // and either winding gives the same pixels
        assert_eq!(covered([&a, &c, &b]).len(), covered([&a, &b, &c]).len());
    }

    #[test]
    fn varyings_interpolate_across_a_triangle() {
        let (a, b, c) = (vertex(0.0, 0.0, 0.0), vertex(16.0, 0.0, 16.0), vertex(0.0, 16.0, 0.0));
        let pixels = covered([&a, &b, &c]);
        let at = |x, y| pixels.iter().find(|&&(px, py, _)| (px, py) == (x, y)).map(|&(_, _, value)| value);
        assert!((at(0, 0).unwrap() - 0.5).abs() < 1e-4);
        assert!((at(7, 3).unwrap() - 7.5).abs() < 1e-4);
    }

    #[test]
    fn perspective_bends_the_interpolation() {
        let near = ClipVertex { position: [0.0, 0.0, 0.0, 1.0], varyings: vec![[0.0; 4]], point_size: None };
        let far = ClipVertex { position: [32.0, 0.0, 0.0, 2.0], varyings: vec![[1.0; 4]], point_size: None };
        let low = ClipVertex { position: [0.0, 16.0, 0.0, 1.0], varyings: vec![[0.0; 4]], point_size: None };
        let pixels = covered([&near, &far, &low]);
        // halfway across the screen is not halfway along the varying
        let middle = pixels.iter().find(|&&(x, y, _)| (x, y) == (8, 0)).unwrap().2;
        assert!(middle < 0.5, "{middle}");
    }

    #[test]
    fn a_triangle_behind_the_eye_is_cut_away() {
        let a = ClipVertex { position: [0.0, 0.0, 0.0, -1.0], varyings: vec![[0.0; 4]], point_size: None };
        let (b, c) = (vertex(8.0, 0.0, 0.0), vertex(0.0, 8.0, 0.0));
        let pixels = covered([&a, &b, &c]);
        assert!(!pixels.is_empty());
        let all_behind = [&a, &a.clone(), &a.clone()];
        assert!(covered(all_behind).is_empty());
    }

    #[test]
    fn winding_tells_the_faces_apart() {
        let (a, b, c) = (vertex(0.0, 0.0, 0.0), vertex(8.0, 0.0, 0.0), vertex(0.0, 8.0, 0.0));
        assert_ne!(is_front_facing([&a, &b, &c], &identity()), is_front_facing([&a, &c, &b], &identity()));
    }

    #[test]
    fn lines_and_points_cover_what_they_should() {
        let bounds = Bounds { x_min: 0, y_min: 0, x_max: 15, y_max: 15 };
        let mut pixels = Vec::new();
        line([&vertex(0.0, 0.5, 0.0), &vertex(8.0, 0.5, 1.0)], &identity(), bounds, 1.0, &mut |fragment| {
            pixels.push((fragment.x, fragment.y))
        });
        assert_eq!(pixels, (0..8).map(|x| (x, 0)).collect::<Vec<_>>());
        let mut count = 0;
        point(&vertex(4.0, 4.0, 0.0), &identity(), bounds, 2.0, &mut |_| count += 1);
        assert_eq!(count, 4);
    }
}
