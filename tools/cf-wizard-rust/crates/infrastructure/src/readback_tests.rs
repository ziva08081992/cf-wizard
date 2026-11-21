use super::*;

#[test]
fn full_readback_rejects_duplicate_expanded_attributes_and_reserved_bindings() {
    let before = br#"<R xmlns:a="urn:u" a:p="kept"/>"#;
    let equivalent = br#"<R xmlns:b="urn:u" b:p="kept"/>"#;
    verify_xml(before, equivalent, None).unwrap();
    for invalid in [
        br#"<R xmlns:a="urn:u" xmlns:b="urn:u" b:p="wrong" a:p="kept"/>"#.as_slice(),
        br#"<R xmlns:a="urn:u" xmlns:xml="urn:other" a:p="kept"/>"#.as_slice(),
    ] {
        assert!(verify_xml(before, invalid, None).is_err());
        assert!(xml_signature(invalid).is_err());
    }
}

#[test]
fn full_readback_preserves_literal_strings_that_look_like_qnames() {
    let before =
        br#"<Root xmlns:p="urn:same"><Comment>p:Text</Comment><Item caption="p:Text"/></Root>"#;
    for after in [
        br#"<Root xmlns:q="urn:same"><Comment>q:Text</Comment><Item caption="p:Text"/></Root>"#
            .as_slice(),
        br#"<Root xmlns:q="urn:same"><Comment>p:Text</Comment><Item caption="q:Text"/></Root>"#
            .as_slice(),
    ] {
        assert!(verify_xml(before, after, None).is_err());
    }
    let typed_before = br#"<Type xmlns="http://v8.1c.ru/8.1/data/core" xmlns:c="urn:config" xmlns:i="http://www.w3.org/2001/XMLSchema-instance" i:type="c:DataType">c:CatalogRef.Target</Type>"#;
    let typed_after = br#"<Type xmlns="http://v8.1c.ru/8.1/data/core" xmlns:q="urn:config" xmlns:x="http://www.w3.org/2001/XMLSchema-instance" x:type="q:DataType">q:CatalogRef.Target</Type>"#;
    verify_xml(typed_before, typed_after, None).unwrap();
    let before = br#"<Value xmlns:i="http://www.w3.org/2001/XMLSchema-instance" xmlns:s="http://www.w3.org/2001/XMLSchema" xmlns:p="urn:data" i:type="s:QName">p:Item</Value>"#;
    let after = br#"<Value xmlns:t="http://www.w3.org/2001/XMLSchema-instance" xmlns:x="http://www.w3.org/2001/XMLSchema" xmlns:q="urn:data" t:type="x:QName">q:Item</Value>"#;
    verify_xml(before, after, None).unwrap();
}

const UUID: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";
const TYPE_ID: &str = "33333333-3333-4333-8333-333333333333";
const VALUE_ID: &str = "44444444-4444-4444-8444-444444444444";
const MEMBER_ID: &str = "55555555-5555-4555-8555-555555555555";

fn descriptor(kind: &str, props: &str, members: &str, standards: bool) -> String {
    let standards = if standards {
        "<xr:StandardAttribute name=\"Code\"><xr:FillChecking>DontCheck</xr:FillChecking></xr:StandardAttribute>"
    } else {
        ""
    };
    format!(
        r#"<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" xmlns:xr="http://v8.1c.ru/8.3/xcf/readable" xmlns:v8="http://v8.1c.ru/8.1/data/core" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:cfg="http://v8.1c.ru/8.1/data/enterprise/current-config">
<{kind} uuid="{UUID}"><InternalInfo><xr:GeneratedType name="{kind}Ref.Example"><xr:TypeId>{TYPE_ID}</xr:TypeId><xr:ValueId>{VALUE_ID}</xr:ValueId></xr:GeneratedType></InternalInfo>
<Properties><Name>Example</Name><Comment>description</Comment><StandardAttributes>{standards}</StandardAttributes>{props}</Properties><ChildObjects>{members}</ChildObjects></{kind}></MetaDataObject>"#
    )
}
fn member(kind: &str, name: &str, extra: &str) -> String {
    format!(
        r#"<{kind} uuid="{MEMBER_ID}"><Properties><Name>{name}</Name><Type><v8:Type>xs:string</v8:Type></Type>{extra}</Properties></{kind}>"#
    )
}
fn key(kind: &str) -> ObjectKey {
    ObjectKey::parse(&format!("{kind}.Example")).unwrap()
}
fn accepts(kind: &str, before: &str, after: &str) {
    verify_xml(before.as_bytes(), after.as_bytes(), Some(&key(kind))).unwrap();
}
fn rejects(kind: &str, before: &str, after: &str) {
    assert!(verify_xml(before.as_bytes(), after.as_bytes(), Some(&key(kind))).is_err());
}

#[test]
fn materialized_standards_pass_as_stub_and_still_fail_as_full() {
    let before = descriptor("Catalog", "", "", false);
    let after = descriptor("Catalog", "", "", true);
    accepts("Catalog", &before, &after);
    assert!(verify_xml(before.as_bytes(), after.as_bytes(), None).is_err());
    verify_xml(after.as_bytes(), after.as_bytes(), None).unwrap();
    accepts(
        "Catalog",
        &before,
        &after.replace("description", "updated note"),
    );
}

#[test]
fn retained_picture_property_is_mandatory_but_other_defaults_remain_optional() {
    let picture = r#"<Picture xmlns:u="http://v8.1c.ru/8.1/data/ui" xmlns:i="http://www.w3.org/2001/XMLSchema-instance" i:type="u:Pict&#117;re" ref="u:Cam&#101;ra"/>"#;
    let before = descriptor("Catalog", picture, "", false);
    let materialized =
        descriptor("Catalog", picture, "", true).replace("description", "new comment");
    accepts("Catalog", &before, &materialized);
    for changed in [
        String::new(),
        picture.replace(" ref=\"u:Cam&#101;ra\"", ""),
        picture.replace("Cam&#101;ra", "Other"),
        picture.replace("Pict&#117;re", "Font"),
        picture.replace(" i:type=\"u:Pict&#117;re\"", ""),
        picture.replace("http://v8.1c.ru/8.1/data/ui", "urn:other"),
    ] {
        rejects(
            "Catalog",
            &before,
            &descriptor("Catalog", &changed, "", true),
        );
    }
    let foreign = picture.replace("Pict&#117;re", "Font");
    accepts(
        "Catalog",
        &descriptor("Catalog", &foreign, "", false),
        &descriptor("Catalog", "", "", true),
    );
}

#[test]
fn picture_default_qnames_aliases_and_entities_are_semantic() {
    let picture = r#"<m:Picture xmlns:m="http://v8.1c.ru/8.3/MDClasses" xmlns="http://v8.1c.ru/8.1/data/ui" xmlns:i="http://www.w3.org/2001/XMLSchema-instance" i:type="Pict&#117;re" ref="Cam&#101;ra"/>"#;
    let aliased = r#"<Picture xmlns:p="http://v8.1c.ru/8.1/data/ui" xmlns:t="http://www.w3.org/2001/XMLSchema-instance" t:type="p:Picture" ref="p:Camera"/>"#;
    let before = descriptor("Catalog", picture, "", false);
    let after = descriptor("Catalog", aliased, "", false);
    accepts("Catalog", &before, &after);
    verify_xml(before.as_bytes(), after.as_bytes(), None).unwrap();
    for changed in [
        picture.replace("http://v8.1c.ru/8.1/data/ui", "urn:other"),
        aliased.replace("ref=\"p:Camera\"", "xmlns:r=\"urn:other\" ref=\"r:Camera\""),
    ] {
        rejects(
            "Catalog",
            &before,
            &descriptor("Catalog", &changed, "", true),
        );
    }
}

#[test]
fn root_identity_namespace_and_generated_identifiers_are_required() {
    let before = descriptor("Catalog", "", "", false);
    let after = descriptor("Catalog", "", "", true);
    for changed in [
        after.replacen(UUID, OTHER, 1),
        after
            .replace("<Catalog ", "<Document ")
            .replace("</Catalog>", "</Document>"),
        after.replace("<Name>Example</Name>", "<Name>Other</Name>"),
        after.replace("http://v8.1c.ru/8.3/MDClasses", "urn:wrong"),
        after.replace(TYPE_ID, OTHER),
        after.replace(VALUE_ID, OTHER),
    ] {
        rejects("Catalog", &before, &changed);
    }
    rejects(
        "Catalog",
        &before,
        &after.replace(&format!(" uuid=\"{UUID}\""), ""),
    );
    rejects(
        "Catalog",
        &before,
        &after.replace(
            "<Name>Example</Name>",
            "<Name>Example</Name><Name>Example</Name>",
        ),
    );
    let missing = before.replace(TYPE_ID, "not-a-uuid");
    rejects("Catalog", &before, &missing);
}

#[test]
fn required_enum_attribute_resource_and_master_dimension_keep_full_content() {
    for (kind, child) in [
        ("Enum", member("EnumValue", "Needed", "")),
        ("Catalog", member("Attribute", "Needed", "")),
        ("AccumulationRegister", member("Resource", "Needed", "")),
        (
            "InformationRegister",
            member("Dimension", "Needed", "<Master>true</Master>"),
        ),
    ] {
        let before = descriptor(kind, "", &child, false);
        let after = descriptor(kind, "", &child, true);
        accepts(kind, &before, &after);
        rejects(kind, &before, &after.replace(&child, ""));
        rejects(kind, &before, &after.replace(MEMBER_ID, OTHER));
        rejects(
            kind,
            &before,
            &after.replace("<Name>Needed</Name>", "<Name>Other</Name>"),
        );
        rejects(kind, &before, &after.replace("xs:string", "xs:decimal"));
    }
}

#[test]
fn nested_table_part_columns_and_their_types_are_required() {
    let column = member("Attribute", "Column", "<Comment> value </Comment>");
    let table = format!(
        r#"<TabularSection uuid="{OTHER}"><Properties><Name>Rows</Name></Properties><ChildObjects>{column}</ChildObjects></TabularSection>"#
    );
    let before = descriptor("Document", "", &table, false);
    let after = descriptor("Document", "", &table, true);
    accepts("Document", &before, &after);
    rejects("Document", &before, &after.replace(&column, ""));
    rejects(
        "Document",
        &before,
        &after.replace("xs:string", "xs:decimal"),
    );
    rejects("Document", &before, &after.replace(" value ", "value"));
    rejects(
        "Document",
        &before,
        &after.replace("http://www.w3.org/2001/XMLSchema", "urn:wrong"),
    );
}

#[test]
fn required_properties_lists_records_and_retained_references_cannot_change() {
    for (kind, props, from, to) in [
        (
            "AccountingRegister",
            "<ChartOfAccounts>ChartOfAccounts.Main</ChartOfAccounts>",
            "ChartOfAccounts.Main",
            "ChartOfAccounts.Other",
        ),
        (
            "DocumentJournal",
            "<RegisteredDocuments><xr:Item>Document.Main</xr:Item></RegisteredDocuments>",
            "Document.Main",
            "Document.Other",
        ),
        (
            "Document",
            "<RegisterRecords><xr:Item>InformationRegister.Main</xr:Item></RegisterRecords>",
            "InformationRegister.Main",
            "InformationRegister.Other",
        ),
        (
            "Document",
            "<BasedOn><xr:Item>Document.Main</xr:Item></BasedOn>",
            "Document.Main",
            "Document.Other",
        ),
        (
            "Constant",
            "<Type><v8:Type>cfg:CatalogRef.Main</v8:Type></Type>",
            "CatalogRef.Main",
            "CatalogRef.Other",
        ),
        (
            "Catalog",
            "<Owners><xr:Item>Catalog.Main</xr:Item></Owners>",
            "Catalog.Main",
            "Catalog.Other",
        ),
    ] {
        let before = descriptor(kind, props, "", false);
        let after = descriptor(kind, props, "", true);
        accepts(kind, &before, &after);
        rejects(kind, &before, &after.replace(from, to));
        rejects(kind, &before, &after.replace(props, ""));
    }
}

#[test]
fn inherited_namespace_aliases_keep_the_same_required_projection() {
    let child = member("Attribute", "Needed", "");
    let before = descriptor("Catalog", "", &child, false);
    let after = descriptor("Catalog", "", &child, true)
        .replace("xr:", "readable:")
        .replace("xmlns:xr=", "xmlns:readable=")
        .replace("xs:", "schema:")
        .replace("xmlns:xs=", "xmlns:schema=")
        .replace("v8:", "core:")
        .replace("xmlns:v8=", "xmlns:core=");
    accepts("Catalog", &before, &after);
}

#[test]
fn malformed_or_wrong_stub_roles_do_not_become_an_allow_rule() {
    let before = descriptor("Catalog", "", "", false);
    for value in ["", "Unknown.Example", "Catalog.Other", "Document.Example"] {
        let key = ObjectKey(value.into());
        assert!(verify_xml(before.as_bytes(), before.as_bytes(), Some(&key)).is_err());
    }
    assert!(verify_xml(before.as_bytes(), b"<broken", Some(&key("Catalog"))).is_err());
}

#[test]
fn signature_preserves_scoped_names_mixed_content_and_xml_whitespace() {
    let xml = "<R xmlns='urn:outer' xmlns:p='urn:parent' xml:space='preserve'>\r\n\
<p:C xmlns:p='urn:child' a='x\t&#9;y\r\nz&#13;' p:b='t'>one\r<![CDATA[two&]]>&amp;</p:C>\r\
<p:C xml:space='default'>  </p:C>\r\n<T xmlns=''><U/></T></R>";
    let preserve = "{http://www.w3.org/XML/1998/namespace}space";
    let signature = json!(["{urn:outer}R", {(preserve): ["text", "preserve"]}, ["text", "\n"], [
        [["{urn:child}C", {"a": ["text", "x \ty z\r"], "{urn:child}b": ["text", "t"]},
            ["text", "one\ntwo&&"], []], ["text", "\n"]],
        [["{urn:parent}C", {(preserve): ["text", "default"]}, null, []], ["text", "\n"]],
        [["T", {}, ["text", ""], [[["U", {}, ["text", ""], []], ["text", ""]]]], ["text", ""]]
    ]]);
    assert_eq!(xml_signature(xml.as_bytes()).unwrap(), signature);
}

#[test]
fn identical_bytes_still_validate_xml_and_stub_requirements() {
    for invalid in [
        b"<R".as_slice(),
        b"<R><p:C/></R>",
        b"<R>&undefined;</R>",
        b"<R>\xff</R>",
        br#"<R xmlns:a="urn:u" xmlns:b="urn:u" a:x="1" b:x="2"/>"#,
    ] {
        assert!(verify_xml(invalid, invalid, None).is_err());
    }
    let duplicate = descriptor("Catalog", "<Name>Example</Name>", "", false);
    rejects("Catalog", &duplicate, &duplicate);
}
