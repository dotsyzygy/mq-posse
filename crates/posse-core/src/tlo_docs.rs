//! Checks that `docs/tlo.md` and `src/tlo.rs` list the same members. The plugin
//! crate can't run tests, so this reads both as text.

use std::collections::{BTreeMap, BTreeSet};

type Members = BTreeMap<String, BTreeSet<String>>;

const TLO_SOURCE: &str = include_str!("../../../src/tlo.rs");
const TLO_DOC: &str = include_str!("../../../docs/tlo.md");

/// How a member is added in `tlo.rs`, directly or with its helpers.
const MEMBER_CALLS: [&str; 3] = [".member(\"", "field(def, \"", "flag(def, \""];

/// Each datatype's members, from its `add_*_type` function.
fn members_in_source(source: &str) -> Members {
    let mut members = Members::new();
    for function in source.split("\nfn add_").skip(1) {
        let Some((_, rest)) = function.split_once("mq.add_type(\"") else { continue };
        let name = rest.split('"').next().unwrap_or_default().to_owned();
        let found = MEMBER_CALLS.iter().flat_map(|call| function.split(call).skip(1)).filter_map(|after| after.split('"').next());
        members.insert(name, found.map(str::to_owned).collect());
    }
    members
}

/// Each datatype's members, from the table rows under its `## `name`` heading.
/// Other headings of that level end the section.
fn members_in_doc(doc: &str) -> Members {
    let mut members = Members::new();
    let mut section = None;
    for line in doc.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            section = heading.strip_prefix('`').and_then(|heading| heading.strip_suffix('`')).filter(|name| name.starts_with("posse")).map(str::to_owned);
            if let Some(name) = &section {
                members.entry(name.clone()).or_default();
            }
        } else if let (Some(name), Some(row)) = (&section, line.strip_prefix("| `.")) {
            let member = row.split(['`', '[']).next().unwrap_or_default();
            members.entry(name.clone()).or_default().insert(member.to_owned());
        }
    }
    members
}

/// What differs between the two, one line each.
fn differences(source: &Members, doc: &Members) -> Vec<String> {
    let mut found = Vec::new();
    for name in source.keys().chain(doc.keys()).collect::<BTreeSet<_>>() {
        match (source.get(name), doc.get(name)) {
            (Some(_), None) => found.push(format!("{name} isn't in docs/tlo.md")),
            (None, Some(_)) => found.push(format!("{name} is in docs/tlo.md but not src/tlo.rs")),
            (Some(in_source), Some(in_doc)) => {
                found.extend(in_source.difference(in_doc).map(|member| format!("{name}.{member} isn't in docs/tlo.md")));
                found.extend(in_doc.difference(in_source).map(|member| format!("{name}.{member} is in docs/tlo.md but not src/tlo.rs")));
            }
            (None, None) => {}
        }
    }
    found
}

#[test]
fn every_member_in_the_source_is_documented_and_nothing_else_is() {
    assert_eq!(differences(&members_in_source(TLO_SOURCE), &members_in_doc(TLO_DOC)), Vec::<String>::new());
}

/// The control for that test: it found the real types and their members, whichever
/// way each was added, so it isn't passing by comparing nothing.
#[test]
fn the_real_source_and_doc_are_read_properly() {
    for members in [members_in_source(TLO_SOURCE), members_in_doc(TLO_DOC)] {
        let names: Vec<&str> = members.keys().map(String::as_str).collect();
        assert_eq!(names, ["posse", "posse_buff", "posse_channel", "posse_last_command", "posse_member", "posse_path", "posse_pet"]);
        for (name, member) in [("posse", "Channel"), ("posse_member", "GameState"), ("posse_member", "Name"), ("posse_member", "Combat"), ("posse_member", "Buff"), ("posse_last_command", "TimedOut")] {
            assert!(members[name].contains(member), "{name}.{member}");
        }
    }
}

/// The names in a `const NAME: &[&str] = &[...];` list.
fn own_list(source: &str, constant: &str) -> BTreeSet<String> {
    let after = source.split(&format!("const {constant}: &[&str] = &[")).nth(1).unwrap_or_default();
    after.split("];").next().unwrap_or_default().split('"').skip(1).step_by(2).map(str::to_owned).collect()
}

const OWN_LISTS: [(&str, &str); 3] = [("MEMBER_OWN", "posse_member"), ("PET_OWN", "posse_pet"), ("BUFF_OWN", "posse_buff")];

/// Each catch-all leaves out exactly its type's own members: `any_member`
/// refuses a list with one of them, and a name left out needlessly is one of
/// MacroQuest's that Lua can't read.
#[test]
fn each_catch_all_leaves_out_exactly_its_types_members() {
    let members = members_in_source(TLO_SOURCE);
    for (constant, type_name) in OWN_LISTS {
        assert_eq!(own_list(TLO_SOURCE, constant), members[type_name], "{constant}");
    }
}

/// Negative: a member added without being added to its list is caught.
#[test]
fn a_member_missing_from_its_list_is_caught() {
    let source = TLO_SOURCE.replacen("\"PathAge\", ", "", 1);
    assert_ne!(source, TLO_SOURCE, "the list has PathAge to remove");
    let members = members_in_source(&source);
    assert_ne!(own_list(&source, "MEMBER_OWN"), members["posse_member"]);
}

const SOURCE: &str = r#"
fn add_thing_type(mq: &Mq<Posse>) {
    let mut def = DataTypeDef::new()
        .member("One", |plugin, _, _, _| None)
        .text(|_, _, _| "text is not a member");
    def = field(def, "Two", |status| None);
    def = flag(def, "Three", flags::THREE);
    mq.add_type("posse_thing", def)
}
"#;

const DOC: &str = "
## `Posse`

| `.NotInASection` | int | The TLO's own forms aren't a datatype |

## `posse_thing`

| Member | Type | Meaning |
|---|---|---|
| `.One` | `int` | Words |
| `.Two[n or name]` | `int` | Words |
| `.Three` | `bool` | Words |

## Not built yet

| `.Planned` | int | Not a member yet |
";

#[test]
fn a_matching_source_and_doc_differ_in_nothing() {
    assert_eq!(differences(&members_in_source(SOURCE), &members_in_doc(DOC)), Vec::<String>::new());
}

#[test]
fn an_undocumented_member_is_found_however_it_was_added() {
    for (call, member) in [(".member(\"One\"", "One"), ("field(def, \"Two\"", "Two"), ("flag(def, \"Three\"", "Three")] {
        let source = SOURCE.replace(call, &call.replace(member, "Added"));
        let found = differences(&members_in_source(&source), &members_in_doc(DOC));
        assert_eq!(found, ["posse_thing.Added isn't in docs/tlo.md".to_owned(), format!("posse_thing.{member} is in docs/tlo.md but not src/tlo.rs")]);
    }
}

#[test]
fn a_documented_member_that_was_removed_is_found() {
    let doc = DOC.replace("| `.One` | `int` | Words |", "| `.One` | `int` | Words |\n| `.Gone` | `int` | Words |");
    assert_eq!(differences(&members_in_source(SOURCE), &members_in_doc(&doc)), ["posse_thing.Gone is in docs/tlo.md but not src/tlo.rs"]);
}

#[test]
fn a_type_missing_from_either_side_is_found() {
    let source = format!("{SOURCE}\nfn add_other_type(mq: &Mq<Posse>) {{\n    mq.add_type(\"posse_other\", def)\n}}\n");
    assert_eq!(differences(&members_in_source(&source), &members_in_doc(DOC)), ["posse_other isn't in docs/tlo.md"]);
    let doc = format!("{DOC}\n## `posse_other`\n");
    assert_eq!(differences(&members_in_source(SOURCE), &members_in_doc(&doc)), ["posse_other is in docs/tlo.md but not src/tlo.rs"]);
}
