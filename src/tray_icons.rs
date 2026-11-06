//! Small, self-contained ARGB icons. Shapes also distinguish states without color.
use crate::tray::IconState;
use std::sync::LazyLock;

pub(crate) fn pixmaps(state: IconState) -> Vec<ksni::Icon> {
    static ICONS: LazyLock<[Vec<ksni::Icon>; 5]> = LazyLock::new(|| {
        [
            IconState::Ready,
            IconState::Listening,
            IconState::Transcribing,
            IconState::Typing,
            IconState::Error,
        ]
        .map(|state| [24, 32, 64].map(|size| render(state, size)).to_vec())
    });
    ICONS[state as usize].clone()
}

fn line(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> bool {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (x - a.0 - t * dx).hypot(y - a.1 - t * dy) < 0.85
}

fn glyph(state: IconState, x: f32, y: f32) -> bool {
    match state {
        IconState::Ready | IconState::Listening => {
            let capsule = (10.0..=14.0).contains(&x) && (5.0..=12.0).contains(&y)
                || (x - 12.0).hypot(y - 5.0) < 2.0
                || (x - 12.0).hypot(y - 12.0) < 2.0;
            let arc = ((x - 12.0).hypot(y - 11.0) - 5.0).abs() < 0.8 && y >= 11.0;
            capsule
                || arc
                || line(x, y, (12.0, 16.0), (12.0, 20.0))
                || line(x, y, (9.0, 20.0), (15.0, 20.0))
                || state == IconState::Listening
                    && (line(x, y, (3.0, 8.0), (3.0, 14.0))
                        || line(x, y, (21.0, 8.0), (21.0, 14.0)))
        }
        IconState::Transcribing => {
            let ring = ((x - 12.0).hypot(y - 12.0) - 8.0).abs() < 1.0;
            ring || line(x, y, (12.0, 6.0), (12.0, 12.0)) || line(x, y, (12.0, 12.0), (16.0, 14.0))
        }
        IconState::Typing => {
            let border = (3.0..=21.0).contains(&x)
                && (6.0..=18.0).contains(&y)
                && (x < 4.5 || x > 19.5 || y < 7.5 || y > 16.5);
            let keys = [6.0, 10.0, 14.0, 18.0]
                .iter()
                .any(|key| (x - key).abs() < 0.9)
                && [10.0, 13.0].iter().any(|row| (y - row).abs() < 0.9);
            border || keys || line(x, y, (8.0, 15.5), (16.0, 15.5))
        }
        IconState::Error => {
            line(x, y, (12.0, 3.0), (22.0, 21.0))
                || line(x, y, (22.0, 21.0), (2.0, 21.0))
                || line(x, y, (2.0, 21.0), (12.0, 3.0))
                || line(x, y, (12.0, 9.0), (12.0, 14.0))
                || (x - 12.0).hypot(y - 17.5) < 1.0
        }
    }
}

fn render(state: IconState, size: i32) -> ksni::Icon {
    let color = match state {
        IconState::Ready => [225, 229, 235],
        IconState::Listening => [76, 217, 138],
        IconState::Transcribing => [91, 174, 255],
        IconState::Typing => [197, 150, 255],
        IconState::Error => [255, 108, 108],
    };
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut coverage = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) * 24.0 / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) * 24.0 / size as f32;
                    coverage += u16::from(glyph(state, x, y));
                }
            }
            data.extend_from_slice(&[(coverage * 255 / 16) as u8, color[0], color[1], color[2]]);
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_state_has_distinct_nonempty_argb_pixmaps() {
        let icons = [
            IconState::Ready,
            IconState::Listening,
            IconState::Transcribing,
            IconState::Typing,
            IconState::Error,
        ]
        .map(pixmaps);
        for variants in &icons {
            for icon in variants {
                assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
                assert!(
                    icon.data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|pixel| pixel[0] == 255)
                );
            }
        }
        for i in 0..icons.len() {
            for j in i + 1..icons.len() {
                assert_ne!(icons[i][0].data, icons[j][0].data);
            }
        }
    }
}
