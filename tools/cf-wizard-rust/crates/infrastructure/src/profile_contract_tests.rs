//! Independent minimal source/model cases for the finite compatibility profile.
//! These assert tool policy and vocabulary, not platform-canonical minimality.
use super::contract_tests::{configuration, root, source};
use super::*;
use crate::{source_query, source_xml::MD};
use cf_domain::source_facts::SourceProof;

fn document(kind: &str, properties: &str) -> SourceDocument {
    let xml = format!(
        "<MetaDataObject xmlns='{MD}'><{kind} uuid='00000000-0000-0000-0000-000000000001'><Properties><Name>Probe</Name>{properties}</Properties><ChildObjects/></{kind}></MetaDataObject>"
    );
    SourceDocument::descriptor(
        &format!("{kind}.Probe"),
        "contract.xml",
        xml.as_bytes(),
        SourceProof {
            identity: "independent-model".into(),
            snapshot: "fixed".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap()
}

#[test]
fn designer_folder_and_class_contract_has_a_case_for_every_supported_pair() {
    // Fixed source-contract rows, independent of descriptor() output.
    let rows = "Catalog Catalogs\nDocument Documents\nEnum Enums\nCommonModule CommonModules\nReport Reports\nDataProcessor DataProcessors\nExchangePlan ExchangePlans\nChartOfAccounts ChartsOfAccounts\nChartOfCharacteristicTypes ChartsOfCharacteristicTypes\nChartOfCalculationTypes ChartsOfCalculationTypes\nBusinessProcess BusinessProcesses\nTask Tasks\nInformationRegister InformationRegisters\nAccumulationRegister AccumulationRegisters\nAccountingRegister AccountingRegisters\nCalculationRegister CalculationRegisters\nCommonForm CommonForms\nCommonCommand CommonCommands\nCommonTemplate CommonTemplates\nCommonPicture CommonPictures\nCommonAttribute CommonAttributes\nRole Roles\nSubsystem Subsystems\nConstant Constants\nDocumentJournal DocumentJournals\nDefinedType DefinedTypes\nSessionParameter SessionParameters\nFunctionalOption FunctionalOptions\nFunctionalOptionsParameter FunctionalOptionsParameters\nEventSubscription EventSubscriptions\nScheduledJob ScheduledJobs\nFilterCriterion FilterCriteria\nSettingsStorage SettingsStorages\nStyleItem StyleItems\nStyle Styles\nLanguage Languages\nXDTOPackage XDTOPackages\nWebService WebServices\nHTTPService HTTPServices\nWSReference WSReferences\nDocumentNumerator DocumentNumerators\nSequence Sequences\nExternalDataSource ExternalDataSources\nBot Bots\nInterface Interfaces\nIntegrationService IntegrationServices\nCommandGroup CommandGroups\nPaletteColor PaletteColors";
    let expected: BTreeSet<_> = rows
        .lines()
        .map(|row| row.split_once(' ').unwrap())
        .collect();
    assert_eq!(expected, TYPE_FOLDERS.iter().copied().collect());
    for (kind, folder) in expected {
        assert_eq!(
            descriptor(&format!("{kind}.Probe")),
            Some(format!("{folder}/Probe.xml"))
        );
        let doc = document(kind, "");
        assert_eq!(doc.view.names[doc.object].uri, MD);
        assert_eq!(doc.view.names[doc.object].local, kind);
        assert!(doc.structural_facts().unwrap().targets.is_empty());
    }
}

#[test]
fn each_query_kind_has_an_independent_static_source_case() {
    let rows = "Справочник Catalog\nДокумент Document\nПеречисление Enum\nРегистрСведений InformationRegister\nРегистрНакопления AccumulationRegister\nРегистрБухгалтерии AccountingRegister\nРегистрРасчета CalculationRegister\nПланВидовХарактеристик ChartOfCharacteristicTypes\nПланСчетов ChartOfAccounts\nПланВидовРасчета ChartOfCalculationTypes\nПланОбмена ExchangePlan\nБизнесПроцесс BusinessProcess\nЗадача Task\nЖурналДокументов DocumentJournal\nКонстанта Constant\nПоследовательность Sequence\nКритерийОтбора FilterCriterion";
    let expected: BTreeSet<_> = rows
        .lines()
        .map(|row| row.split_once(' ').unwrap())
        .collect();
    assert_eq!(
        expected,
        crate::kinds::QUERY_KINDS.iter().copied().collect()
    );
    for (label, kind) in expected {
        let key = format!("{kind}.Entries");
        let facts = source_query::read(
            &format!("SELECT R.Value FROM {label}.Entries AS R"),
            &mut BTreeMap::new(),
        );
        assert_eq!(
            facts.paths,
            vec![(key.clone(), vec![]), (key.clone(), vec!["Value".into()])]
        );
        assert_eq!(facts.required, BTreeSet::from([key]));
    }
}

#[test]
fn each_member_token_is_a_member_and_not_a_predefined_value() {
    let labels = "AccountingFlag AddressingAttribute Attribute Channel Column Command Dimension EnumValue ExtDimensionAccountingFlag Form Method Operation Recalculation Resource TabularSection Template URLTemplate";
    let expected: BTreeSet<_> = labels.split_whitespace().collect();
    assert_eq!(
        expected,
        crate::kinds::MEMBER_KEYWORDS.iter().copied().collect()
    );
    for member in expected {
        let values = crate::reference_lexer::references(&format!("Catalog.Owner.{member}.Chosen"));
        assert_eq!(values.len(), 1);
        assert_eq!(
            (
                values[0].key(),
                values[0].member.as_str(),
                values[0].member_name.as_str(),
                values[0].predefined.as_str()
            ),
            ("Catalog.Owner".into(), member, "Chosen", "")
        );
    }
}

#[test]
fn virtual_table_rows_and_plain_third_sections_have_different_path_meanings() {
    let labels = "Balance BalanceAndTurnovers Changes DrCrTurnovers ExtDimensions RecordsWithExtDimensions ScheduleData SliceFirst SliceLast Turnovers ДанныеГрафика ДвиженияССубконто Изменения Обороты ОборотыДтКт Остатки ОстаткиИОбороты СрезПервых СрезПоследних Субконто";
    let expected: BTreeSet<_> = labels.split_whitespace().collect();
    assert_eq!(
        expected,
        crate::kinds::VIRTUAL_TABLES.iter().copied().collect()
    );
    for label in expected {
        let query = format!("SELECT R.Amount FROM InformationRegister.Entries.{label}() AS R");
        assert_eq!(
            source_query::read(&query, &mut BTreeMap::new()).paths,
            vec![
                ("InformationRegister.Entries".into(), vec![]),
                ("InformationRegister.Entries".into(), vec!["Amount".into()])
            ]
        );
    }
    assert_eq!(
        source_query::read(
            "SELECT R.Amount FROM Document.Invoice.Rows AS R",
            &mut BTreeMap::new()
        )
        .paths,
        vec![
            ("Document.Invoice".into(), vec!["Rows".into()]),
            (
                "Document.Invoice".into(),
                vec!["Rows".into(), "Amount".into()]
            )
        ]
    );
}

#[test]
fn keyword_profile_suppresses_standalone_words_but_not_qualified_fields() {
    // A finite lexical profile, not a claim that every word is reserved by 1C.
    let labels = "all allowed and as asc autoorder avg between by case cast count desc distinct drop else end escape false for from full group having hierarchy in index inner into is isnull join left like max min not null on only or order outer overall periods presentation refpresentation refs right select substring sum then top totals true type undefined union update value valuetype when where автоупорядочивание в внешнее внутреннее возр все выбор выбрать выразить где для есть естьnull значение и иерархии из изменения или имеющие иначе индексировать истина итоги как когда количество конец левое ложь максимум между минимум не неопределено общие объединить первые периодами по подобно подстрока полное поместить правое представление представлениессылки различные разрешенные сгруппировать соединение спецсимвол среднее ссылка сумма тип типзначения тогда только убыв уничтожить упорядочить";
    let expected: BTreeSet<_> = labels.split_whitespace().collect();
    assert_eq!(
        expected,
        crate::kinds::QUERY_KEYWORDS.iter().copied().collect()
    );
    for label in expected {
        let query = format!("SELECT {label} FROM Catalog.Entries AS R");
        assert_eq!(
            source_query::read(&query, &mut BTreeMap::new()).paths,
            vec![("Catalog.Entries".into(), vec![])],
            "{label}"
        );
        let query = format!("SELECT R.{label} FROM Catalog.Entries AS R");
        assert_eq!(
            source_query::read(&query, &mut BTreeMap::new()).paths,
            vec![
                ("Catalog.Entries".into(), vec![]),
                ("Catalog.Entries".into(), vec![label.into()])
            ],
            "{label}"
        );
    }
    for label in ["AS", "As", "as", "КАК", "Как", "как"] {
        assert_eq!(
            source_query::read(
                &format!("SELECT R.Value FROM Catalog.Entries {label} R"),
                &mut BTreeMap::new()
            )
            .paths,
            vec![
                ("Catalog.Entries".into(), vec![]),
                ("Catalog.Entries".into(), vec!["Value".into()])
            ]
        );
    }
}

#[test]
fn every_resource_and_side_suffix_resolves_only_an_existing_base_member() {
    let suffixes = "НачальныйРазвернутыйОстаток КонечныйРазвернутыйОстаток OpeningSplittedBalance ClosingSplittedBalance НачальныйОстаток КонечныйОстаток ВстречныйОборот OpeningBalance ClosingBalance CorrTurnover Turnover Остаток Balance Receipt Expense Оборот Приход Расход";
    assert_eq!(
        suffixes.split_whitespace().collect::<BTreeSet<_>>(),
        RESOURCE_SUFFIXES.iter().copied().collect()
    );
    let members = vec![Member {
        name: "Amount".into(),
        tag: "Resource".into(),
        types: BTreeSet::new(),
        columns: vec![],
    }];
    for suffix in suffixes.split_whitespace() {
        for side in ["", "Дт", "Кт", "Dr", "Cr"] {
            let field = format!("Amount{suffix}{side}");
            assert_eq!(register_member(&members, &field).0, "Amount", "{field}");
            assert_eq!(register_member(&members, &fold(&field)).0, "Amount");
            assert!(register_member(&[], &field).1.is_none());
        }
        assert!(
            register_member(&members, suffix).1.is_none(),
            "empty base cannot match"
        );
    }
    for side in ["Дт", "Кт", "Dr", "Cr"] {
        assert_eq!(
            register_member(&members, &format!("Amount{side}")).0,
            "Amount"
        );
    }
    let ordinary = Member {
        name: "AmountBalance".into(),
        tag: "Attribute".into(),
        types: BTreeSet::new(),
        columns: vec![],
    };
    assert_eq!(
        find_member(&[ordinary], "AmountBalance").0,
        "AmountBalance",
        "an exact ordinary member wins before suffix adaptation"
    );
}

#[test]
fn all_four_register_kinds_and_information_register_fallbacks_have_minimum_cases() {
    for (kind, field, mode) in [
        ("InformationRegister", "Resource", "Independent"),
        ("InformationRegister", "Dimension", "Independent"),
        ("InformationRegister", "Attribute", "Independent"),
        ("InformationRegister", "Resource", "RecorderSubordinate"),
        ("AccumulationRegister", "Resource", "Independent"),
        ("AccountingRegister", "Resource", "Independent"),
        ("CalculationRegister", "Resource", "Independent"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let register = format!(
            "<MetaDataObject><{kind}><Properties><Name>Data</Name><WriteMode>{mode}</WriteMode></Properties><ChildObjects><{field}><Properties><Name>Value</Name></Properties></{field}></ChildObjects></{kind}></MetaDataObject>"
        );
        let file = descriptor(&format!("{kind}.Data")).unwrap();
        let recorder = format!(
            "<MetaDataObject xmlns:r='http://v8.1c.ru/8.3/xcf/readable'><Document><Properties><Name>Recorder</Name><RegisterRecords><r:Item>{kind}.Data</r:Item></RegisterRecords></Properties></Document></MetaDataObject>"
        );
        let src = source(
            &[
                (
                    "Configuration.xml",
                    configuration(&format!(
                        "<CommonModule>Root</CommonModule><{kind}>Data</{kind}><Document>Recorder</Document>"
                    )),
                ),
                ("CommonModules/Root.xml", root(&format!("{kind}.Data"))),
                (&file, register),
                ("Documents/Recorder.xml", recorder),
            ],
            &dir.path().join("source"),
        );
        let recipe = prepare(
            &src,
            None,
            &BTreeSet::from(["CommonModule.Root".into()]),
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(
            recipe
                .composition
                .choices
                .iter()
                .any(|c| c.rule == "minimum_field"
                    && c.chosen == format!("{kind}.Data.{field}.Value")),
            "{kind}.{field}"
        );
        assert_eq!(
            recipe.composition.stubs.contains_key("Document.Recorder"),
            kind != "InformationRegister" || mode == "RecorderSubordinate"
        );
    }
}

#[test]
fn all_required_scalar_properties_survive_projection_and_strict_readback() {
    let cases = [
        ("FunctionalOption", "Location"),
        ("BusinessProcess", "Task"),
        ("AccountingRegister", "ChartOfAccounts"),
        ("CalculationRegister", "ChartOfCalculationTypes"),
        ("CalculationRegister", "Schedule"),
        ("CalculationRegister", "ScheduleDate"),
        ("CalculationRegister", "ScheduleValue"),
        ("ChartOfAccounts", "ExtDimensionTypes"),
        ("CommonCommand", "Group"),
        ("CommonAttribute", "DataSeparationUse"),
        ("CommonAttribute", "DataSeparationValue"),
        ("EventSubscription", "Handler"),
        ("EventSubscription", "Source"),
        ("ScheduledJob", "MethodName"),
    ];
    assert_eq!(
        cases.into_iter().collect::<BTreeSet<_>>(),
        crate::kinds::REQUIRED_PROPERTIES
            .iter()
            .flat_map(|(kind, names)| names.iter().map(move |name| (*kind, *name)))
            .collect()
    );
    for (kind, property) in cases {
        let body = if property == "DataSeparationUse" {
            "Use"
        } else {
            "Catalog.Required"
        };
        let doc = document(kind, &format!("<{property}>{body}</{property}>"));
        let selection = crate::stubs::StubSpec {
            finalized: true,
            ..Default::default()
        };
        let plan = crate::projection::compile(
            &doc,
            &format!("{kind}.Probe"),
            &selection,
            &BTreeSet::new(),
        )
        .unwrap();
        let bytes = crate::source_edits::emit(doc.view.xml.source.as_bytes(), &plan.edits).unwrap();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains(&format!("<{property}>{body}</{property}>")),
            "{kind}.{property}"
        );
        let changed = std::str::from_utf8(&bytes).unwrap().replace(
            &format!("<{property}>{body}</{property}>"),
            &format!("<{property}/>"),
        );
        crate::readback::verify_xml(
            &bytes,
            &bytes,
            Some(&cf_domain::ObjectKey::parse(&format!("{kind}.Probe")).unwrap()),
        )
        .unwrap();
        assert!(
            crate::readback::verify_xml(
                &bytes,
                changed.as_bytes(),
                Some(&cf_domain::ObjectKey::parse(&format!("{kind}.Probe")).unwrap())
            )
            .is_err(),
            "{kind}.{property}"
        );
    }
}

#[test]
fn every_omitted_module_protection_distinguishes_empty_bom_and_nonempty_content() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let modules = [
        "ObjectModule",
        "ManagerModule",
        "RecordSetModule",
        "ValueManagerModule",
        "CommandModule",
        "Module",
    ];
    assert_eq!(
        modules.into_iter().collect::<BTreeSet<_>>(),
        crate::kinds::STUB_MODULES.iter().copied().collect()
    );
    for module in modules {
        for (payload, protected) in [
            ("", false),
            ("\u{feff} \r\n", false),
            ("// existing omitted module", true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut row = tests::rows()["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == "required-values")
                .unwrap()
                .clone();
            let path = format!("Catalogs/Тест/Ext/{module}.bsl");
            row["source"][&path] = serde_json::json!(STANDARD.encode(payload));
            let src = tests::source(&row, &dir.path().join("source"), false);
            let selected = serde_json::from_value(row["selected"].clone()).unwrap();
            let recipe = prepare(
                &src,
                None,
                &selected,
                &BTreeSet::new(),
                &Cancellation::default(),
            )
            .unwrap();
            let actual = recipe
                .merge_rules
                .get("Catalog.Тест")
                .is_some_and(|rules| rules.iter().any(|rule| rule == module));
            assert_eq!(actual, protected, "{module} {payload:?}");
            assert!(
                !recipe
                    .compiled
                    .as_ref()
                    .unwrap()
                    .projection
                    .files
                    .contains_key(&path)
            );
            std::fs::write(src.local.root.join(path), "// changed after preview").unwrap();
            assert!(
                recipe
                    .write(
                        &src,
                        &BTreeSet::new(),
                        &dir.path().join("stage"),
                        &dir.path().join("settings.xml"),
                        &Cancellation::default()
                    )
                    .is_err()
            );
            assert!(!dir.path().join("stage").exists());
        }
    }
}

#[cfg(windows)]
#[test]
fn local_omitted_modules_with_case_inexact_paths_keep_protection_and_stale_hash_checks() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    for path in [
        "Catalogs/Тест/ext/objectmodule.bsl",
        "Catalogs/Тест/ext/ObjectModule.bsl",
        "Catalogs/Тест/Ext/objectmodule.bsl",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut row = tests::rows()["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "required-values")
            .unwrap()
            .clone();
        row["source"][path] = serde_json::json!(STANDARD.encode("// original omitted module"));
        let src = tests::source(&row, &dir.path().join("source"), false);
        let recipe = prepare(
            &src,
            None,
            &serde_json::from_value(row["selected"].clone()).unwrap(),
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(
            recipe.merge_rules["Catalog.Тест"]
                .iter()
                .any(|s| s == "ObjectModule"),
            "{path}"
        );
        assert!(
            recipe
                .composition
                .source_hashes
                .contains_key("Catalogs/Тест/Ext/ObjectModule.bsl")
        );
        recipe
            .write(
                &src,
                &BTreeSet::new(),
                &dir.path().join("stage-ok"),
                &dir.path().join("settings-ok.xml"),
                &Cancellation::default(),
            )
            .unwrap();
        std::fs::write(src.local.root.join(path), "// changed after preview").unwrap();
        assert!(
            recipe
                .write(
                    &src,
                    &BTreeSet::new(),
                    &dir.path().join("stage-stale"),
                    &dir.path().join("settings-stale.xml"),
                    &Cancellation::default()
                )
                .is_err()
        );
        assert!(!dir.path().join("stage-stale").exists());
    }
}

#[test]
fn pinned_git_module_lookup_does_not_inherit_windows_path_case_aliases() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let dir = tempfile::tempdir().unwrap();
    let mut row = tests::rows()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "required-values")
        .unwrap()
        .clone();
    row["source"]["Catalogs/Тест/ext/objectmodule.bsl"] =
        serde_json::json!(STANDARD.encode("// different pinned blob path"));
    let src = tests::source(&row, &dir.path().join("source"), true);
    let recipe = prepare(
        &src,
        None,
        &serde_json::from_value(row["selected"].clone()).unwrap(),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(
        !recipe
            .merge_rules
            .get("Catalog.Тест")
            .is_some_and(|v| v.iter().any(|s| s == "ObjectModule"))
    );
    assert!(
        !recipe
            .composition
            .source_hashes
            .contains_key("Catalogs/Тест/Ext/ObjectModule.bsl")
    );
}

fn open_source(
    directory: &std::path::Path,
    default: &str,
    schema: &str,
    declarations: &str,
) -> tests::Source {
    source(&[
        ("Configuration.xml", configuration("<CommonModule>Root</CommonModule><Report>Target</Report><Catalog>Hidden</Catalog>")),
        ("CommonModules/Root.xml", root("Report.Target.StandardCommand.Open Report.Target.Command.Action")),
        ("Reports/Target.xml", format!("<MetaDataObject><Report><Properties><Name>Target</Name><DefaultForm>{default}</DefaultForm><MainDataCompositionSchema>{schema}</MainDataCompositionSchema></Properties><ChildObjects>{declarations}<Command><Properties><Name>Action</Name></Properties></Command></ChildObjects></Report></MetaDataObject>")),
        ("Reports/Target/Forms/Main.xml", "<MetaDataObject><Form><Properties><Name>Main</Name></Properties></Form></MetaDataObject>".into()),
        ("Reports/Target/Forms/Main/Ext/Form.xml", "<Form/>".into()),
        ("Reports/Target/Forms/Main/Ext/Form/Module.bsl", "x = VALUE(Catalog.Hidden.EmptyRef);".into()),
        ("Reports/Target/Templates/Schema.xml", "<MetaDataObject><Template><Properties><Name>Schema</Name></Properties></Template></MetaDataObject>".into()),
        ("Reports/Target/Templates/Schema/Ext/Template.bin", "synthetic opaque payload".into()),
        ("Reports/Target/Commands/Action/Ext/Command.xml", "<Command/>".into()),
        ("Reports/Target/Commands/Action/Ext/CommandModule.bsl", "x = VALUE(Catalog.Hidden.EmptyRef);".into()),
        ("Catalogs/Hidden.xml", "<MetaDataObject><Catalog><Properties><Name>Hidden</Name></Properties></Catalog></MetaDataObject>".into()),
    ], directory)
}

#[test]
fn open_properties_keep_both_own_declarations_and_dependency_modules_stay_omitted() {
    let dir = tempfile::tempdir().unwrap();
    let src = open_source(
        &dir.path().join("source"),
        "Report.Target.Form.Main",
        "Report.Target.Template.Schema",
        "<Form>Main</Form><Template>Schema</Template>",
    );
    let selected = BTreeSet::from(["CommonModule.Root".into()]);
    let recipe = prepare(
        &src,
        None,
        &selected,
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(
        recipe.composition.complete(),
        "{:?}",
        recipe.composition.diagnostics
    );
    let files = &recipe.compiled.as_ref().unwrap().projection.files;
    for path in [
        "Reports/Target/Forms/Main.xml",
        "Reports/Target/Templates/Schema.xml",
    ] {
        assert!(files.contains_key(path));
    }
    for path in [
        "Reports/Target/Forms/Main/Ext/Form/Module.bsl",
        "Reports/Target/Commands/Action/Ext/CommandModule.bsl",
    ] {
        assert!(!files.contains_key(path));
    }
    assert_eq!(recipe.merge_rules["Report.Target.Form.Main"], vec!["Form"]);
    assert_eq!(
        recipe.merge_rules["Report.Target.Command.Action"],
        vec!["CommandModule"]
    );
    assert!(!recipe.composition.stubs.contains_key("Catalog.Hidden"));
    let full = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into(), "Report.Target".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    for path in [
        "Reports/Target/Forms/Main/Ext/Form/Module.bsl",
        "Reports/Target/Commands/Action/Ext/CommandModule.bsl",
    ] {
        assert!(
            full.compiled
                .as_ref()
                .unwrap()
                .projection
                .files
                .contains_key(path)
        );
    }
    assert!(full.composition.stubs.contains_key("Catalog.Hidden"));
}

#[test]
fn open_property_cannot_require_an_undeclared_own_form() {
    let dir = tempfile::tempdir().unwrap();
    let src = open_source(
        &dir.path().join("source"),
        "Report.Target.Form.Absent",
        "",
        "",
    );
    let recipe = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(!recipe.composition.complete());
    assert!(recipe.composition.diagnostics.iter().any(
        |d| d.kind == DiagnosticKind::MissingMember && d.target == "Report.Target.Form.Absent"
    ));
}

#[test]
fn open_foreign_form_property_is_removed_instead_of_importing_its_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = open_source(
        &dir.path().join("source"),
        "Catalog.Hidden.Form.Other",
        "",
        "",
    );
    let recipe = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(recipe.composition.complete());
    assert!(!recipe.composition.stubs.contains_key("Catalog.Hidden"));
}

#[test]
fn required_list_fallback_prefers_included_item_and_reports_empty_input() {
    for (included, empty, expected) in [
        (false, false, Some(0)),
        (true, false, Some(1)),
        (false, true, None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let items = if empty {
            ""
        } else {
            "<r:Item>Document.A</r:Item><r:Item>Document.B</r:Item>"
        };
        let src = source(&[
            ("Configuration.xml", configuration("<CommonModule>Root</CommonModule><DocumentJournal>Entries</DocumentJournal><Document>A</Document><Document>B</Document>")),
            ("CommonModules/Root.xml", root(if included { "DocumentJournal.Entries Document.B" } else { "DocumentJournal.Entries" })),
            ("DocumentJournals/Entries.xml", format!("<MetaDataObject xmlns:r='http://v8.1c.ru/8.3/xcf/readable'><DocumentJournal><Properties><Name>Entries</Name><RegisteredDocuments>{items}</RegisteredDocuments></Properties></DocumentJournal></MetaDataObject>")),
            ("Documents/A.xml", "<MetaDataObject><Document><Properties><Name>A</Name></Properties></Document></MetaDataObject>".into()),
            ("Documents/B.xml", "<MetaDataObject><Document><Properties><Name>B</Name></Properties></Document></MetaDataObject>".into()),
        ], &dir.path().join("source"));
        let recipe = prepare(
            &src,
            None,
            &BTreeSet::from(["CommonModule.Root".into()]),
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
        if let Some(ordinal) = expected {
            assert!(recipe.composition.complete());
            let output = recipe.built["DocumentJournal.Entries"]
                .output()
                .unwrap()
                .descriptor;
            let (kept, dropped) = if ordinal == 0 {
                ("Document.A", "Document.B")
            } else {
                ("Document.B", "Document.A")
            };
            assert!(output.contains(&format!("<r:Item>{kept}</r:Item>")));
            assert!(!output.contains(&format!("<r:Item>{dropped}</r:Item>")));
            if !included {
                assert_eq!(
                    recipe
                        .composition
                        .choices
                        .iter()
                        .filter(|c| c.rule == "required_list")
                        .map(|c| c.ordinal)
                        .collect::<Vec<_>>(),
                    vec![ordinal]
                );
            }
        } else {
            assert!(!recipe.composition.complete());
            assert!(
                recipe
                    .composition
                    .diagnostics
                    .iter()
                    .any(|d| d.kind == DiagnosticKind::MissingCandidate)
            );
        }
    }
}

#[test]
fn effective_reference_path_policy_preserves_english_russian_and_case_variants() {
    for word in ["Ref", "ref", "REF", "Ссылка", "ссылка", "ССЫЛКА"] {
        let dir = tempfile::tempdir().unwrap();
        let src = source(&[("Configuration.xml", configuration("<CommonModule>Root</CommonModule><Catalog>Target</Catalog>")), ("CommonModules/Root.xml", root("")), ("Catalogs/Target.xml", "<MetaDataObject><Catalog><Properties><Name>Target</Name></Properties><ChildObjects><Attribute><Properties><Name>Shared</Name></Properties></Attribute></ChildObjects></Catalog></MetaDataObject>".into())], &dir.path().join("source"));
        let selected = BTreeSet::from(["CommonModule.Root".into()]);
        let excluded = BTreeSet::new();
        let cancel = Cancellation::default();
        let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
        session
            .walk(
                &BTreeSet::from(["Catalog.Target".into()]),
                &[word.into(), "Shared".into()],
                0,
                "CommonModule.Root",
                0,
            )
            .unwrap();
        session.closure().unwrap();
        assert!(
            session.built["Catalog.Target"]
                .output()
                .unwrap()
                .descriptor
                .contains("<Name>Shared</Name>"),
            "{word}"
        );
        session
            .walk(
                &BTreeSet::from(["Catalog.Target".into()]),
                &[word.into()],
                0,
                "CommonModule.Root",
                0,
            )
            .unwrap();
    }
}

#[test]
fn data_processor_open_keeps_both_supported_own_properties() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&[
        ("Configuration.xml", configuration("<CommonModule>Root</CommonModule><DataProcessor>Target</DataProcessor>")),
        ("CommonModules/Root.xml", root("DataProcessor.Target.StandardCommand.Open")),
        ("DataProcessors/Target.xml", "<MetaDataObject><DataProcessor><Properties><Name>Target</Name><DefaultForm>DataProcessor.Target.Form.Main</DefaultForm><MainDataCompositionSchema>DataProcessor.Target.Template.Schema</MainDataCompositionSchema></Properties><ChildObjects><Form>Main</Form><Template>Schema</Template></ChildObjects></DataProcessor></MetaDataObject>".into()),
        ("DataProcessors/Target/Forms/Main.xml", "<MetaDataObject><Form><Properties><Name>Main</Name></Properties></Form></MetaDataObject>".into()),
        ("DataProcessors/Target/Templates/Schema.xml", "<MetaDataObject><Template><Properties><Name>Schema</Name></Properties></Template></MetaDataObject>".into()),
    ], &dir.path().join("source"));
    let recipe = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(recipe.composition.complete());
    let files = &recipe.compiled.as_ref().unwrap().projection.files;
    assert!(files.contains_key("DataProcessors/Target/Forms/Main.xml"));
    assert!(files.contains_key("DataProcessors/Target/Templates/Schema.xml"));
}
