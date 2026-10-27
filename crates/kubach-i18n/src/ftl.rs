//! Messages of a `.ftl` file and the variables each one takes. Shared by
//! the build script (keys checked at compile time) and the tests
//! (every locale has the keys and arguments of English).

use std::collections::{BTreeMap, BTreeSet};

use fluent_syntax::ast::{Entry, Expression, InlineExpression, Pattern, PatternElement};

/// Message id → names of the variables (`$name`) its value uses, in
/// selectors too. Terms (`-brand`) and attributes are not messages.
pub type Messages = BTreeMap<String, BTreeSet<String>>;

/// Parses `source`; `Err` lists the syntax errors and duplicate ids.
pub fn messages(source: &str) -> Result<Messages, String> {
    let resource =
        fluent_syntax::parser::parse(source).map_err(|(_, errors)| format!("{errors:?}"))?;
    let mut messages = Messages::new();
    for entry in resource.body {
        if let Entry::Message(message) = entry {
            let mut args = BTreeSet::new();
            if let Some(value) = &message.value {
                pattern_args(value, &mut args);
            }
            let id = message.id.name.to_owned();
            if messages.insert(id.clone(), args).is_some() {
                return Err(format!("message `{id}` is defined twice"));
            }
        }
    }
    Ok(messages)
}

fn pattern_args(pattern: &Pattern<&str>, args: &mut BTreeSet<String>) {
    for element in &pattern.elements {
        if let PatternElement::Placeable { expression } = element {
            expression_args(expression, args);
        }
    }
}

fn expression_args(expression: &Expression<&str>, args: &mut BTreeSet<String>) {
    match expression {
        Expression::Select { selector, variants } => {
            inline_args(selector, args);
            for variant in variants {
                pattern_args(&variant.value, args);
            }
        }
        Expression::Inline(inline) => inline_args(inline, args),
    }
}

fn inline_args(inline: &InlineExpression<&str>, args: &mut BTreeSet<String>) {
    match inline {
        InlineExpression::VariableReference { id } => {
            args.insert(id.name.to_owned());
        }
        InlineExpression::FunctionReference { arguments, .. } => {
            for positional in &arguments.positional {
                inline_args(positional, args);
            }
            for named in &arguments.named {
                inline_args(&named.value, args);
            }
        }
        InlineExpression::Placeable { expression } => expression_args(expression, args),
        _ => {}
    }
}
