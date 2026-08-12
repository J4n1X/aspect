//! By-value containment cycle detection for type-struct/sum layouts.
//!
//! A layout that stores itself by value — directly, through a chain of other
//! by-value fields, or through an array — is infinite and must be rejected
//! before codegen could ever recurse on one. Any pointer indirection breaks
//! the cycle, since a pointer's size doesn't depend on its pointee's layout.

use crate::lexer::{LangType, TypeBase};
use crate::symbol::ids::{StructId, SumId};
use crate::symbol::module::ModuleSymbols;
use std::collections::HashMap;

/// One by-value link in the type graph: a type-struct or sum whose layout may
/// recursively contain another by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Node {
    Struct(StructId),
    Sum(SumId),
}

fn edges(module: &ModuleSymbols, node: Node) -> Vec<Node> {
    let field_types: Vec<LangType> = match node {
        Node::Struct(id) => module[id]
            .fields
            .iter()
            .map(|f| f.ty)
            .collect(),
        Node::Sum(id) => module[id]
            .variants
            .iter()
            .flat_map(|v| v.fields.iter().map(|(_, ty)| *ty))
            .collect(),
    };
    field_types
        .into_iter()
        .filter(|ty| ty.pointer_depth == 0)
        .filter_map(|ty| match ty.base {
            TypeBase::Struct(id) => Some(Node::Struct(id)),
            TypeBase::Sum(id) => Some(Node::Sum(id)),
            _ => None,
        })
        .collect()
}

// Colors: absent = unvisited, false = on the current DFS path (gray),
// true = fully explored (black). A gray re-entry closes a cycle.
fn visit(module: &ModuleSymbols, colors: &mut HashMap<Node, bool>, cyclic: &mut Vec<Node>, node: Node) {
    match colors.get(&node) {
        Some(false) => {
            if !cyclic.contains(&node) {
                cyclic.push(node);
            }
            return;
        }
        Some(true) => return,
        None => {}
    }
    colors.insert(node, false);
    for next in edges(module, node) {
        visit(module, colors, cyclic, next);
    }
    colors.insert(node, true);
}

/// Every type-struct/sum whose layout closes a by-value cycle, walking from
/// every struct/sum in `module` as a DFS root.
pub(crate) fn find_byvalue_cycles(module: &ModuleSymbols) -> Vec<Node> {
    let mut colors = HashMap::new();
    let mut cyclic = Vec::new();
    let roots: Vec<Node> = module
        .structs()
        .map(|(id, _)| Node::Struct(id))
        .chain(module.sums().map(|(id, _)| Node::Sum(id)))
        .collect();
    for root in roots {
        visit(module, &mut colors, &mut cyclic, root);
    }
    cyclic
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Position;
    use crate::symbol::module::{
        FieldInfo, StructBody, SumBody, SumVariant, TypeKind, Visibility,
    };

    fn intern_struct(module: &mut ModuleSymbols, name: &str) -> StructId {
        let id = module.intern_type(
            name,
            0,
            Visibility::Private,
            Position::new(0, 0),
            TypeKind::Struct(StructBody::default()),
        );
        module.as_struct(id).expect("interned as a type-struct")
    }

    fn intern_sum(module: &mut ModuleSymbols, name: &str) -> SumId {
        let id = module.intern_type(
            name,
            0,
            Visibility::Private,
            Position::new(0, 0),
            TypeKind::Sum(SumBody::default()),
        );
        module.as_sum(id).expect("interned as a sum")
    }

    fn field(name: &str, ty: LangType) -> FieldInfo {
        FieldInfo {
            name: name.to_string(),
            ty,
            vis: Visibility::Private,
        }
    }

    #[test]
    fn struct_containing_itself_is_cyclic() {
        let mut module = ModuleSymbols::new();
        let s = intern_struct(&mut module, "S");
        module.set_fields(s.def(), vec![field("s", LangType::struct_type(s))]);

        let cyclic = find_byvalue_cycles(&module);
        assert_eq!(cyclic, vec![Node::Struct(s)]);
    }

    // The DFS reports the node it re-enters while still gray — here `A`,
    // reached first as a root — not every member of the cycle it closes.
    #[test]
    fn mutual_struct_sum_cycle_is_detected() {
        let mut module = ModuleSymbols::new();
        let a = intern_struct(&mut module, "A");
        let b = intern_sum(&mut module, "B");
        module.set_fields(a.def(), vec![field("b", LangType::sum_type(b))]);
        module.set_sum_variants(
            b.def(),
            vec![SumVariant {
                name: "Wraps".to_string(),
                fields: vec![("a".to_string(), LangType::struct_type(a))],
            }],
        );

        let cyclic = find_byvalue_cycles(&module);
        assert_eq!(cyclic, vec![Node::Struct(a)]);
    }

    #[test]
    fn pointer_field_breaks_the_cycle() {
        let mut module = ModuleSymbols::new();
        let s = intern_struct(&mut module, "S");
        let self_ptr = LangType {
            pointer_depth: 1,
            ..LangType::struct_type(s)
        };
        module.set_fields(s.def(), vec![field("next", self_ptr)]);

        assert!(find_byvalue_cycles(&module).is_empty());
    }
}
