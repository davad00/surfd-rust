use base64::{Engine, engine::general_purpose::STANDARD};
use bevy_math::{DVec3, Vec3};
use serde::Deserialize;

/// The original BSP's packed potentially visible sets, in Source coordinates.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualVisibility {
    format: u32,
    bsp_sha1: String,
    cluster_count: usize,
    planes: Vec<[f64; 4]>,
    nodes: Vec<[i32; 3]>,
    pvs: Vec<String>,
}

impl VisualVisibility {
    pub(crate) fn parse(value: serde_json::Value, bsp_sha1: &str) -> Option<Self> {
        let table: Self = serde_json::from_value(value).ok()?;
        if table.format != 1
            || table.bsp_sha1 != bsp_sha1
            || !(1..=65_536).contains(&table.cluster_count)
            || table.pvs.len() != table.cluster_count
            || table.nodes.is_empty()
            || !table.nodes.iter().all(|&[plane, front, back]| {
                (0..table.planes.len() as i32).contains(&plane)
                    && [front, back].into_iter().all(|child| {
                        (-(table.cluster_count as i32) - 1..table.nodes.len() as i32)
                            .contains(&child)
                    })
            })
        {
            return None;
        }
        Some(table)
    }

    fn cluster_at<const CHECK_CORNERS: bool>(&self, position: DVec3) -> Option<(usize, bool)> {
        let mut node = 0;
        let mut clear = CHECK_CORNERS;
        for _ in 0..self.nodes.len() {
            let [plane, front, back] = self.nodes[node];
            let [x, y, z, distance] = self.planes[plane as usize];
            let projection = position.x * x + position.y * y + position.z * z;
            if clear {
                // The eight fallback points are the corners of a two-unit cube.
                // Keep the full lookup when any corner may cross this BSP plane.
                let radius = x.abs() + y.abs() + z.abs();
                let rounding = (position.x.abs() * x.abs()
                    + position.y.abs() * y.abs()
                    + position.z.abs() * z.abs()
                    + distance.abs()
                    + radius)
                    * (8.0 * f64::EPSILON);
                clear = (projection - distance).abs() > radius + rounding;
            }
            let child = if projection >= distance { front } else { back };
            if child < 0 {
                return usize::try_from(-child - 2)
                    .ok()
                    .map(|cluster| (cluster, clear));
            }
            node = child as usize;
        }
        None
    }

    pub fn clusters_near(&self, eye: Vec3) -> Option<[usize; 9]> {
        if !eye.is_finite() {
            return None;
        }
        let point = DVec3::new(eye.x.into(), (-eye.z).into(), eye.y.into());
        let (center, clear) = self.cluster_at::<true>(point)?;
        let mut clusters = [center; 9];
        if clear {
            return Some(clusters);
        }
        for (corner, cluster) in clusters.iter_mut().skip(1).enumerate() {
            let step = |mask| if corner & mask == 0 { -1.0 } else { 1.0 };
            *cluster = self
                .cluster_at::<false>(point + DVec3::new(step(1), step(2), step(4)))?
                .0;
        }
        clusters.sort_unstable();
        let mut unique = 1;
        for index in 1..clusters.len() {
            if clusters[index] != clusters[unique - 1] {
                clusters[unique] = clusters[index];
                unique += 1;
            }
        }
        let last = clusters[unique - 1];
        clusters[unique..].fill(last);
        Some(clusters)
    }

    pub fn merge_rows(
        &self,
        clusters: &[usize; 9],
        mask: &mut Vec<u8>,
        encoded: &mut Vec<u8>,
    ) -> bool {
        let width = self.cluster_count.div_ceil(8);
        mask.resize(width, 0);
        mask.fill(0);
        let mut previous = None;
        for &cluster in clusters {
            if previous == Some(cluster) {
                continue;
            }
            previous = Some(cluster);
            encoded.clear();
            if self
                .pvs
                .get(cluster)
                .is_none_or(|row| STANDARD.decode_vec(row, encoded).is_err())
                || !merge_source_row(encoded, mask)
            {
                return false;
            }
            mask[cluster / 8] |= 1 << (cluster % 8);
        }
        true
    }

    pub fn target_visible(&self, mask: &[u8], clusters: &[usize]) -> bool {
        clusters.is_empty()
            || clusters.iter().any(|&cluster| {
                cluster >= self.cluster_count
                    || mask
                        .get(cluster / 8)
                        .is_none_or(|byte| byte & (1 << (cluster % 8)) != 0)
            })
    }
}

fn merge_source_row(mut row: &[u8], mask: &mut [u8]) -> bool {
    let mut position = 0;
    while let Some((&byte, remaining)) = row.split_first() {
        if byte == 0 {
            let Some((&run, remaining)) = remaining.split_first() else {
                return false;
            };
            if run == 0 || position + usize::from(run) > mask.len() {
                return false;
            }
            position += usize::from(run);
            row = remaining;
        } else {
            let Some(output) = mask.get_mut(position) else {
                return false;
            };
            *output |= byte;
            position += 1;
            row = remaining;
        }
    }
    position == mask.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_unions_boundary_clusters_and_rejects_a_truncated_row() {
        let mut table = VisualVisibility::parse(
            serde_json::json!({
                "format": 1, "bspSha1": "fixture", "clusterCount": 2,
                "planes": [[1.0, 0.0, 0.0, 0.0]], "nodes": [[0, -2, -3]],
                "pvs": ["AQ==", "Ag=="]
            }),
            "fixture",
        )
        .unwrap();
        let mut mask = Vec::new();
        let mut scratch = Vec::new();
        let near = table.clusters_near(Vec3::ZERO).unwrap();
        assert!(table.merge_rows(&near, &mut mask, &mut scratch));
        assert_eq!(mask, [0b11]);
        let inside = table.clusters_near(Vec3::X * 3.0).unwrap();
        assert!(table.merge_rows(&inside, &mut mask, &mut scratch));
        assert_eq!(mask, [0b01]);
        table.pvs[1] = "AA==".to_owned();
        assert!(!table.merge_rows(&near, &mut mask, &mut scratch));
    }

    #[test]
    fn checks_other_clusters_at_exact_plane_corners() {
        let table = VisualVisibility::parse(
            serde_json::json!({
                "format": 1, "bspSha1": "fixture", "clusterCount": 3,
                "planes": [[1.0, 2.0, 1.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
                "nodes": [[0, 1, -3], [1, -2, -4]],
                "pvs": ["AQ==", "Ag==", "BA=="]
            }),
            "fixture",
        )
        .unwrap();
        for x in -10..=10 {
            for y in -5..=5 {
                let eye = Vec3::new(x as f32 * 0.5, y as f32 * 0.5, -0.5);
                let point = DVec3::new(eye.x.into(), (-eye.z).into(), eye.y.into());
                let mut expected = vec![table.cluster_at::<false>(point).unwrap().0];
                for corner in 0..8 {
                    let offset = |mask| if corner & mask == 0 { -1.0 } else { 1.0 };
                    expected.push(
                        table
                            .cluster_at::<false>(
                                point + DVec3::new(offset(1), offset(2), offset(4)),
                            )
                            .unwrap()
                            .0,
                    );
                }
                expected.sort_unstable();
                expected.dedup();
                let mut actual = table.clusters_near(eye).unwrap().to_vec();
                actual.dedup();
                assert_eq!(actual, expected);
            }
        }
    }
}
