//! Deterministic Sphinx-style C++ IDs (Phase 13).
//!
//! Port of the common subset of `sphinx/domains/cpp/_ids.py`: the `_CPPv4`
//! prefix plus an Itanium-style mangling of the declaration.  Full SFINAE and
//! complete Itanium mangling are tracked gaps (ARCHITECTURE.md §17); the subset
//! here covers names, nested names, cv/pointer/reference qualifiers, builtin
//! types, templates and simple function parameter lists.

/// One signature parameter reduced to its mangling components.
#[derive(Clone, Debug, Default)]
pub struct MangleParam {
    /// Base type spelling (e.g. `int`, `Sphinx`, `std::vector<int>`).
    pub ty: String,
    /// Pointer depth.
    pub pointers: usize,
    /// Reference kind: none, `&` or `&&`.
    pub reference: u8,
    /// `true` for a const-qualified type.
    pub is_const: bool,
}

/// A declaration to mangle.
#[derive(Clone, Debug, Default)]
pub struct MangleDecl {
    /// Nested name components (e.g. `["Sphinx", "version"]`).
    pub names: Vec<String>,
    /// Function parameter types (empty for objects).
    pub params: Vec<MangleParam>,
    /// Template arguments of the final name (e.g. `["int"]` for `f<int>`).
    pub template_args: Vec<String>,
}

/// Itanium-style substitution for a builtin type, or `None` for named types.
fn builtin_code(ty: &str) -> Option<&'static str> {
    Some(match ty.trim() {
        "void" => "v",
        "bool" | "_Bool" => "b",
        "char" => "c",
        "signed char" => "a",
        "unsigned char" => "h",
        "short" | "short int" | "signed short" => "s",
        "unsigned short" | "unsigned short int" => "t",
        "int" | "signed" | "signed int" => "i",
        "unsigned" | "unsigned int" => "j",
        "long" | "long int" | "signed long" => "l",
        "unsigned long" | "unsigned long int" => "m",
        "long long" | "long long int" => "x",
        "unsigned long long" | "unsigned long long int" => "y",
        "float" => "f",
        "double" => "d",
        "long double" => "e",
        "wchar_t" => "w",
        "size_t" => "m",
        "..." => "z",
        _ => return None,
    })
}

fn mangle_named(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    format!("{}{}", cleaned.chars().count(), cleaned)
}

fn mangle_param(param: &MangleParam) -> String {
    let mut out = String::new();
    if param.is_const {
        out.push('K');
    }
    for _ in 0..param.pointers {
        out.push('P');
    }
    match param.reference {
        1 => out.push('R'),
        2 => out.push('O'),
        _ => {}
    }
    let base = if let Some(code) = builtin_code(&param.ty) {
        code.to_owned()
    } else {
        // Named (possibly qualified) type.
        let parts: Vec<&str> = param.ty.split("::").collect();
        if parts.len() > 1 {
            let mut nested = String::from("N");
            for part in parts {
                nested.push_str(&mangle_named(part));
            }
            nested.push('E');
            nested
        } else {
            mangle_named(&param.ty)
        }
    };
    out.push_str(&base);
    out
}

fn mangle_name(decl: &MangleDecl) -> String {
    let mut components: Vec<String> = decl
        .names
        .iter()
        .map(|name| mangle_named(name))
        .collect();
    if !decl.template_args.is_empty() {
        let mut template = String::from("I");
        for arg in &decl.template_args {
            template.push_str(&mangle_param(&MangleParam { ty: arg.clone(), ..MangleParam::default() }));
        }
        template.push('E');
        if let Some(last) = components.last_mut() {
            last.push_str(&template);
        }
    }
    if components.len() > 1 {
        format!("N{}E", components.concat())
    } else {
        components.concat()
    }
}

/// Returns the Sphinx-style IDs for a declaration, in upstream order:
/// `_CPPv4… _CPPv3… _CPPv2… <non-mangled fallback>`.
pub fn cpp_ids(decl: &MangleDecl) -> Vec<String> {
    let name = mangle_name(decl);
    let mangled = if decl.params.is_empty() {
        name.clone()
    } else {
        let params: String = decl.params.iter().map(mangle_param).collect();
        format!("{name}{params}")
    };
    // For nested names Itanium wraps the parameters inside the `N…E` scope.
    let mut ids = if decl.names.len() > 1 && !decl.params.is_empty() {
        let mut components: Vec<String> = decl.names.iter().map(|name| mangle_named(name)).collect();
        if !decl.template_args.is_empty() {
            let mut template = String::from("I");
            for arg in &decl.template_args {
                template.push_str(&mangle_param(&MangleParam { ty: arg.clone(), ..MangleParam::default() }));
            }
            template.push('E');
            if let Some(last) = components.last_mut() { last.push_str(&template); }
        }
        let params: String = decl.params.iter().map(mangle_param).collect();
        let nested = format!("_CPPv4N{}{}E", components.concat(), params);
        vec![
            nested.clone(),
            nested.replacen("_CPPv4", "_CPPv3", 1),
            nested.replacen("_CPPv4", "_CPPv2", 1),
        ]
    } else {
        vec![
            format!("_CPPv4{mangled}"),
            format!("_CPPv3{mangled}"),
            format!("_CPPv2{mangled}"),
        ]
    };
    // Upstream's last alias is a human-readable `<name>__<type>` fallback;
    // when no parameters are present it is the bare name.
    if let Some(last) = decl.names.last() {
        if decl.params.is_empty() {
            ids.push(last.clone());
        } else {
            let types: Vec<String> = decl.params.iter().map(|param| param.ty.clone()).collect();
            ids.push(format!("{}__{}", last, types.join("")));
        }
    }
    ids
}

/// The primary ID (the one used for object registration).
pub fn primary_id(decl: &MangleDecl) -> String {
    cpp_ids(decl).into_iter().next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mangles_builtin_function() {
        let decl = MangleDecl {
            names: vec!["hello".into()],
            params: vec![MangleParam { ty: "char".into(), pointers: 1, ..MangleParam::default() }],
            template_args: vec![],
        };
        assert_eq!(primary_id(&decl), "_CPPv45helloPc");
    }

    #[test]
    fn mangles_nested_member() {
        let decl = MangleDecl {
            names: vec!["Sphinx".into(), "version".into()],
            params: vec![],
            template_args: vec![],
        };
        assert_eq!(primary_id(&decl), "_CPPv4N6Sphinx7versionE");
    }

    #[test]
    fn emits_four_aliases() {
        let decl = MangleDecl { names: vec!["A".into()], ..MangleDecl::default() };
        let ids = cpp_ids(&decl);
        assert_eq!(ids.len(), 4);
        assert_eq!(ids[3], "A");
    }
}
