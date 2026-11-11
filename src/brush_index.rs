use std::ops::Range;

use bevy_math::Vec3;

use crate::course::Brush;

const MIN_INDEXED_BRUSHES: usize = 128;
const LEAF_SIZE: usize = 12;

/// Static bounds hierarchy over a course's original brush indices.
pub(crate) struct BrushIndex {
    nodes: Vec<Node>,
    order: Vec<usize>,
    brush_address: usize,
}

struct Node {
    min: Vec3,
    max: Vec3,
    kind: NodeKind,
}

enum NodeKind {
    Leaf(Range<usize>),
    Branch { right: usize },
}

impl BrushIndex {
    pub(crate) fn new(brushes: &[Brush]) -> Option<Self> {
        if brushes.len() < MIN_INDEXED_BRUSHES
            || brushes.iter().any(|brush| {
                !brush.min.is_finite()
                    || !brush.max.is_finite()
                    || !brush.min.cmple(brush.max).all()
            })
        {
            return None;
        }
        let mut nodes = Vec::new();
        let mut order: Vec<_> = (0..brushes.len()).collect();
        Self::build(&mut nodes, &mut order, brushes, 0);
        Some(Self {
            nodes,
            order,
            brush_address: brushes.as_ptr() as usize,
        })
    }

    pub(crate) fn covers(&self, brushes: &[Brush]) -> bool {
        self.order.len() == brushes.len() && self.brush_address == brushes.as_ptr() as usize
    }

    /// An existing tree also works when edited bounds stay inside their old leaves.
    pub(crate) fn covers_bounds(&self, brushes: &[Brush]) -> bool {
        self.covers(brushes)
            && self.nodes.iter().all(|node| {
                let NodeKind::Leaf(range) = &node.kind else {
                    return true;
                };
                self.order[range.clone()].iter().all(|&index| {
                    let brush = &brushes[index];
                    brush.min.cmpge(node.min).all()
                        && brush.max.cmple(node.max).all()
                        && brush.min.cmple(brush.max).all()
                })
            })
    }

    pub(crate) fn query(&self, min: Vec3, max: Vec3, candidates: &mut Vec<usize>) {
        candidates.clear();
        self.collect(0, min, max, candidates);
        // The trace's first contact wins equal-fraction ties.
        candidates.sort_unstable();
    }

    fn collect(&self, index: usize, min: Vec3, max: Vec3, candidates: &mut Vec<usize>) {
        let node = &self.nodes[index];
        if max.cmplt(node.min).any() || min.cmpgt(node.max).any() {
            return;
        }
        match &node.kind {
            NodeKind::Leaf(range) => candidates.extend_from_slice(&self.order[range.clone()]),
            NodeKind::Branch { right } => {
                self.collect(index + 1, min, max, candidates);
                self.collect(*right, min, max, candidates);
            }
        }
    }

    fn build(
        nodes: &mut Vec<Node>,
        order: &mut [usize],
        brushes: &[Brush],
        offset: usize,
    ) -> usize {
        let (min, max) = order.iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(min, max), &index| (min.min(brushes[index].min), max.max(brushes[index].max)),
        );
        let index = nodes.len();
        nodes.push(Node {
            min,
            max,
            kind: NodeKind::Leaf(offset..offset + order.len()),
        });
        if order.len() > LEAF_SIZE {
            let extent = max - min;
            let axis = (0..3)
                .max_by(|&a, &b| extent[a].total_cmp(&extent[b]))
                .expect("three axes");
            let center =
                |index: usize| brushes[index].min[axis] * 0.5 + brushes[index].max[axis] * 0.5;
            let middle = order.len() / 2;
            order.select_nth_unstable_by(middle, |&a, &b| center(a).total_cmp(&center(b)));
            let (left_order, right_order) = order.split_at_mut(middle);
            Self::build(nodes, left_order, brushes, offset);
            let right = Self::build(nodes, right_order, brushes, offset + middle);
            nodes[index].kind = NodeKind::Branch { right };
        }
        index
    }
}
