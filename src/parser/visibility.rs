use crate::lexer::Position;
use crate::parser::{Parser, ParserError};
use crate::symbol::module::{TypeKind, Visibility};

impl Parser {
    /// The module the file `file_id` belongs to. Files without an entry —
    /// including every file when no module info was threaded — belong to the
    /// anonymous root module `""`.
    pub(crate) fn module_of_file(&self, file_id: u32) -> &str {
        self.file_modules
            .get(file_id as usize)
            .map_or("", String::as_str)
    }

    /// Enforce import visibility for one resolved reference: a symbol defined
    /// in a file of module N may be referenced from a file of module M iff
    /// `N == M` or N is a *direct* import of M (imports do not trickle down).
    /// `def_file_id` is the symbol's defining file; `use_pos` is the use site
    /// (whose `file_id` determines the referring module).
    pub(crate) fn check_import_visibility(
        &self,
        kind: &'static str,
        name: &str,
        def_file_id: u32,
        use_pos: Position,
    ) -> Result<(), ParserError> {
        let def_module = self.module_of_file(def_file_id);
        let use_module = self.module_of_file(use_pos.file_id);
        if def_module == use_module
            || self
                .module_imports
                .get(use_module)
                .is_some_and(|imports| imports.iter().any(|import| import == def_module))
        {
            return Ok(());
        }
        Err(ParserError::not_imported(
            kind, name, def_module, use_module, use_pos,
        ))
    }

    /// Two gates for naming any type-struct, enum or sum (or calling a
    /// type-struct's methods): the general import rule, plus a cross-module use
    /// additionally requiring `public`. A member's own `public` is capped by the
    /// type's — a public method of a private type is module-visible only. Values
    /// of a foreign private type may still *flow* through outside code.
    ///
    /// An alias carries no visibility of its own yet, so only the import rule
    /// gates it; its target is checked where the alias resolves.
    pub(crate) fn check_type_visibility(&self, id: u32, use_pos: Position) -> Result<(), ParserError> {
        let def = self.module.type_def(id);
        self.check_import_visibility(def.noun(), &def.name, def.file_id, use_pos)?;
        if matches!(def.kind, TypeKind::Alias(_)) {
            return Ok(());
        }
        let def_module = self.module_of_file(def.file_id);
        let use_module = self.module_of_file(use_pos.file_id);
        if def.vis == Visibility::Private && def_module != use_module {
            return Err(ParserError::private_named_type(
                &def.kind,
                &def.name,
                def_module,
                use_module,
                use_pos,
            ));
        }
        Ok(())
    }

    /// The free-function/global analogue of [`Self::check_struct_visibility`]:
    /// the import rule plus a cross-module use requiring `public`.
    ///
    /// Mangled method names (containing `$`) are exempt from the `public` gate:
    /// a method's cross-module reach is governed by its type's visibility and
    /// its own `MethodSig.vis`, checked in the type checker.
    pub(crate) fn check_name_visibility(
        &self,
        kind: &'static str,
        name: &str,
        def_file_id: u32,
        vis: Visibility,
        use_pos: Position,
    ) -> Result<(), ParserError> {
        self.check_import_visibility(kind, name, def_file_id, use_pos)?;
        if name.contains('$') {
            return Ok(());
        }
        let def_module = self.module_of_file(def_file_id);
        let use_module = self.module_of_file(use_pos.file_id);
        if vis == Visibility::Private && def_module != use_module {
            return Err(ParserError::private_symbol(
                kind, name, def_module, use_module, use_pos,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{Token, TokenKind};
    use std::collections::HashMap;

    /// A parser over an empty token stream with a three-file module
    /// registry: file 0 is the anonymous root module `""` (imports `mid`),
    /// file 1 is `mid` (imports `hidden`), file 2 is `hidden` (imports
    /// nothing).
    fn parser_with_modules() -> Parser {
        let modules = vec![
            (0, String::new()),
            (1, "mid".to_string()),
            (2, "hidden".to_string()),
        ];
        let imports = HashMap::from([
            (String::new(), vec!["mid".to_string()]),
            ("mid".to_string(), vec!["hidden".to_string()]),
            ("hidden".to_string(), Vec::new()),
        ]);
        let eof = Token::new(TokenKind::Eof, Position::new(0, 0), String::new());
        Parser::new(vec![eof]).with_module_info(modules, imports)
    }

    /// A use-site position inside the file with `file_id`.
    fn site(file_id: u32) -> Position {
        Position::with_file(3, 7, file_id)
    }

    #[test]
    fn same_module_references_are_always_visible() {
        let p = parser_with_modules();
        for file in 0..3 {
            assert!(p
                .check_import_visibility("function", "f", file, site(file))
                .is_ok());
        }
    }

    #[test]
    fn directly_imported_modules_are_visible() {
        let p = parser_with_modules();
        // The root imports `mid`; `mid` imports `hidden`.
        assert!(p
            .check_import_visibility("function", "f", 1, site(0))
            .is_ok());
        assert!(p
            .check_import_visibility("function", "f", 2, site(1))
            .is_ok());
    }

    #[test]
    fn transitive_imports_are_not_visible() {
        let p = parser_with_modules();
        let err = p
            .check_import_visibility("function", "gcd_u64", 2, site(0))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "function 'gcd_u64' is defined in module 'hidden', \
             which the root module does not import at 3:7"
        );
        assert_eq!(err.position(), Some(site(0)));
    }

    #[test]
    fn nothing_imports_the_root_module() {
        let p = parser_with_modules();
        let err = p
            .check_import_visibility("global variable", "counter", 0, site(1))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "global variable 'counter' is defined in the root module, \
             which module 'mid' does not import at 3:7"
        );
    }

    #[test]
    fn importing_does_not_grant_visibility_in_reverse() {
        let p = parser_with_modules();
        // `mid` imports `hidden` — `hidden` must not see `mid`.
        assert!(p
            .check_import_visibility("function", "f", 1, site(2))
            .is_err());
    }

    #[test]
    fn without_module_info_every_file_is_the_root_module() {
        let eof = Token::new(TokenKind::Eof, Position::new(0, 0), String::new());
        let p = Parser::new(vec![eof]);
        assert!(p
            .check_import_visibility("function", "f", 4, site(9))
            .is_ok());
    }

    #[test]
    fn private_type_is_module_visible_only() {
        use crate::symbol::module::{StructBody, TypeKind, Visibility};
        let mut p = parser_with_modules();
        let id = p.module.intern_type("Secret", 1, Visibility::Private, Position::new(0, 0), TypeKind::Struct(StructBody::default()));
        // Inside its own module the type is freely usable.
        assert!(p.check_type_visibility(id, site(1)).is_ok());
        // From an importing module, privacy blocks it.
        let err = p.check_type_visibility(id, site(0)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "type-struct 'Secret' is private to module 'mid' and cannot be used from \
             the root module — declare it `public type` to export it at 3:7"
        );
        assert_eq!(err.position(), Some(site(0)));
    }

    #[test]
    fn public_type_is_visible_to_importers_only() {
        use crate::symbol::module::{StructBody, TypeKind, Visibility};
        let mut p = parser_with_modules();
        let id = p.module.intern_type("Pair", 1, Visibility::Public, Position::new(0, 0), TypeKind::Struct(StructBody::default()));
        // The root imports `mid`, so the exported type is visible there.
        assert!(p.check_type_visibility(id, site(0)).is_ok());
        // `public` does not bypass the import rule: `hidden` does not import `mid`.
        assert!(p.check_type_visibility(id, site(2)).is_err());
    }
}
