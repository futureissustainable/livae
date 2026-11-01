//! The Windows panel glass in plain pixels: a screen copy shrunk `SCALE` times, box-blurred three times (close to a
//! Gaussian of `SIGMA_DIP`) and dimmed; and that blur stretched back to full size under a black wash. Pixels are
//! BGRA, top row first; alpha comes out 255.

/// The blur works on a copy this many times smaller in each direction.
pub const SCALE: usize = 4;
/// About the Gaussian sigma of the blur, in DIPs.
pub const SIGMA_DIP: f32 = 18.0;

/// A blurred picture at 1/SCALE size.
pub struct Small {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

/// `data` (`w` x `h`, at `scale` pixels per DIP) shrunk SCALE times, blurred and dimmed to `dim` (1 keeps it as
/// bright).
pub fn blur(data: &[u8], w: usize, h: usize, scale: f32, dim: f32) -> Small {
    let k = SCALE;
    let (sw, sh) = (w.div_ceil(k), h.div_ceil(k));
    if sw == 0 || sh == 0 || data.len() < w * h * 4 {
        return Small { w: 0, h: 0, px: Vec::new() };
    }
    // Shrink: the average of each k x k block.
    let mut a = vec![[0f32; 3]; sw * sh];
    for sy in 0..sh {
        let rows = sy * k..((sy + 1) * k).min(h);
        for sx in 0..sw {
            let cols = sx * k..((sx + 1) * k).min(w);
            // Whole-number sums are exact (at most k * k * 255), so they match adding floats one by one.
            let mut acc = [0u32; 3];
            for yy in rows.clone() {
                for p in data[(yy * w + cols.start) * 4..(yy * w + cols.end) * 4].as_chunks::<4>().0 {
                    for c in 0..3 {
                        acc[c] += p[c] as u32;
                    }
                }
            }
            let n = (rows.len() * cols.len()) as f32;
            a[sy * sw + sx] = acc.map(|v| v as f32 / n.max(1.0));
        }
    }
    // Three box blurs approach a Gaussian; sigma of three passes of radius r is about 1.12 r.
    let r = ((SIGMA_DIP * scale / k as f32) / 1.12).round().max(1.0) as usize;
    let mut b = vec![[0f32; 3]; a.len()];
    let mut acc = vec![[0f32; 3]; sw];
    for _ in 0..3 {
        box_rows(&a, &mut b, sw, r);
        box_columns(&b, &mut a, sw, sh, r, &mut acc);
    }
    let px = a.iter().flat_map(|p| [(p[0] * dim) as u8, (p[1] * dim) as u8, (p[2] * dim) as u8, 255]).collect();
    Small { w: sw, h: sh, px }
}

/// One box-blur pass along each row, edges clamped.
fn box_rows(src: &[[f32; 3]], out: &mut [[f32; 3]], w: usize, r: usize) {
    let span = (2 * r + 1) as f32;
    for (s, o) in src.chunks_exact(w).zip(out.chunks_exact_mut(w)) {
        let mut acc = [0f32; 3];
        for i in 0..=2 * r {
            let p = s[i.saturating_sub(r).min(w - 1)];
            for c in 0..3 {
                acc[c] += p[c];
            }
        }
        for i in 0..w {
            o[i] = acc.map(|v| v / span);
            let (add, sub) = (s[(i + r + 1).min(w - 1)], s[i.saturating_sub(r)]);
            for c in 0..3 {
                acc[c] += add[c] - sub[c];
            }
        }
    }
}

/// One box-blur pass down each column, edges clamped. It walks row by row with one running sum per column, so
/// memory is read in order; each column's sums are the same as walking it alone.
fn box_columns(src: &[[f32; 3]], out: &mut [[f32; 3]], w: usize, h: usize, r: usize, acc: &mut [[f32; 3]]) {
    let span = (2 * r + 1) as f32;
    let row = |y: usize| &src[y * w..(y + 1) * w];
    acc.fill([0f32; 3]);
    for i in 0..=2 * r {
        for (a, p) in acc.iter_mut().zip(row(i.saturating_sub(r).min(h - 1))) {
            for c in 0..3 {
                a[c] += p[c];
            }
        }
    }
    for i in 0..h {
        let (add, sub) = (row((i + r + 1).min(h - 1)), row(i.saturating_sub(r)));
        for x in 0..w {
            out[i * w + x] = acc[x].map(|v| v / span);
            for c in 0..3 {
                acc[x][c] += add[x][c] - sub[x][c];
            }
        }
    }
}

impl Small {
    /// The blur back at `w` x `h` pixels (bilinear), under `wash` black, as BGRA with alpha 255.
    pub fn full_size(&self, w: usize, h: usize, wash: f32) -> Vec<u8> {
        let mut out = vec![0u8; w * h * 4];
        if self.w == 0 || self.h == 0 {
            return out;
        }
        let keep = 1.0 - wash.clamp(0.0, 1.0);
        let (fx, fy) = (self.w as f32 / w.max(1) as f32, self.h as f32 / h.max(1) as f32);
        // Where each output column samples: two source columns and the weight of the second.
        let taps: Vec<(usize, usize, f32)> = (0..w)
            .map(|x| {
                let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (self.w - 1) as f32);
                let x0 = sx as usize;
                (x0 * 4, (x0 + 1).min(self.w - 1) * 4, sx.fract())
            })
            .collect();
        // A source row stretched across the output width; consecutive output rows reuse the same two.
        let stretch = |y: usize, line: &mut Vec<[f32; 3]>| {
            let src = &self.px[y * self.w * 4..(y + 1) * self.w * 4];
            line.clear();
            line.extend(taps.iter().map(|&(a, b, t)| std::array::from_fn(|c| src[a + c] as f32 * (1.0 - t) + src[b + c] as f32 * t)));
        };
        let (mut top, mut bottom) = (Vec::with_capacity(w), Vec::with_capacity(w));
        let mut have = (usize::MAX, usize::MAX);
        for (y, o) in out.chunks_exact_mut(w * 4).enumerate() {
            let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (self.h - 1) as f32);
            let (y0, ty) = (sy as usize, sy.fract());
            let y1 = (y0 + 1).min(self.h - 1);
            if have != (y0, y1) {
                if have.1 == y0 {
                    std::mem::swap(&mut top, &mut bottom);
                } else {
                    stretch(y0, &mut top);
                }
                stretch(y1, &mut bottom);
                have = (y0, y1);
            }
            for ((p, t), b) in o.as_chunks_mut::<4>().0.iter_mut().zip(&top).zip(&bottom) {
                for c in 0..3 {
                    p[c] = ((t[c] * (1.0 - ty) + b[c] * ty) * keep) as u8;
                }
                p[3] = 255;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_picture_stays_flat_and_dims() {
        let (w, h) = (37, 23);
        let data: Vec<u8> = (0..w * h).flat_map(|_| [200, 100, 50, 0]).collect();
        let s = blur(&data, w, h, 1.0, 0.5);
        assert_eq!((s.w, s.h), (10, 6));
        assert!(s.px.as_chunks::<4>().0.iter().all(|p| *p == [100, 50, 25, 255]));
        let full = s.full_size(w, h, 0.5);
        assert_eq!(full.len(), w * h * 4);
        assert!(full.as_chunks::<4>().0.iter().all(|p| *p == [50, 25, 12, 255]));
        assert!(blur(&[], 0, 0, 1.0, 1.0).full_size(4, 4, 0.0).iter().all(|&v| v == 0));
    }
}
