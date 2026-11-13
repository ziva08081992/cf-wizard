"""Development oracle only. The shipped Rust executable never invokes Python."""
from pathlib import Path
import hashlib
import json
import runpy
import sqlite3
import sys
import zlib
import html

fixtures = Path(__file__).resolve().parent
repo = fixtures.parents[3]
project = repo.parents[2]
sys.path.insert(0, str(repo / "tools/1c-delivery"))
from cf_wizard.dependency_index import _facts, SCHEMA, APPLICATION_ID
from cf_wizard.git_index_inputs import GitIndexInputs, GitIndexEntry

helper_path = project / ".agents/skills/1c-partial-cf/scripts/partial_cf.py"
assert hashlib.sha256(helper_path.read_bytes()).hexdigest() == "efe67eda5f95475757efefd75c979d8144ffd7480934307db487155c87ea5699"
helper = runpy.run_path(str(helper_path), run_name="legacy_oracle")
xml = ('<?xml version="1.0" encoding="UTF-8"?>\r\n'
    '<MetaDataObject><Catalog uuid="11111111-1111-4111-8111-111111111111"><Properties><Name>Тест</Name></Properties><ChildObjects>'
    '<Attribute uuid="22222222-2222-4222-8222-222222222222"><Properties><Name>Ссылка</Name><Type><Type>cfg:CatalogRef.Номенклатура</Type><Type>xs:string</Type></Type></Properties></Attribute>'
    '<TabularSection><Properties><Name>Строки</Name></Properties><ChildObjects><Attribute><Properties><Name>Тип</Name><Type><Type>cfg:DefinedType.Составной</Type></Type></Properties></Attribute></ChildObjects></TabularSection>'
    '</ChildObjects></Catalog></MetaDataObject>').encode("utf-8")
configuration = b'<MetaDataObject><Configuration><Properties><Name>Fixture</Name></Properties></Configuration></MetaDataObject>'
(fixtures / "Catalog.Тест.xml").write_bytes(xml)
def blob(data):
    return hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
entries = (
    GitIndexEntry("Configuration.xml", blob(configuration), len(configuration), "", ("configuration",)),
    GitIndexEntry("Catalogs/Тест.xml", blob(xml), len(xml), "Catalog.Тест", ("descriptor",)),
)
inputs = GitIndexInputs("git:fixture", "a" * 40, entries)
facts = _facts(xml, helper)
for mode in ("local", "git"):
    path = fixtures / f"legacy-v1-{mode}.sqlite3"
    assert path.resolve().parent == fixtures
    path.unlink(missing_ok=True)
    with sqlite3.connect(path) as db:
        db.execute(f"PRAGMA application_id={APPLICATION_ID}")
        db.executescript(SCHEMA)
        meta = {"schema": "1", "source": "fixture-local" if mode == "local" else "old-materialization",
                "helper": "efe67eda5f95475757efefd75c979d8144ffd7480934307db487155c87ea5699"}
        if mode == "git":
            meta.update(inputs.context, indexed_revision=inputs.revision)
            db.execute("CREATE TABLE git_inputs(path TEXT PRIMARY KEY,oid TEXT,size INTEGER,owner TEXT,roles TEXT) WITHOUT ROWID")
            db.executemany("INSERT INTO git_inputs VALUES (?,?,?,?,?)", (e.row() for e in entries))
        db.executemany("INSERT INTO metadata VALUES (?,?)", meta.items())
        db.execute("INSERT INTO objects VALUES (?,?,?)", ("Catalog.Тест", hashlib.sha256(xml).digest(), facts))
        db.execute("INSERT INTO documents VALUES (?,?)", ("Событие.xml", hashlib.sha256(b"").digest()))
        db.execute("INSERT INTO registrar_links VALUES (?,?,?)", ("Событие.xml", 0, "AccumulationRegister.Остатки"))
data = {"helper_sha256": meta["helper"], "descriptor_sha256": hashlib.sha256(xml).hexdigest(),
        "facts": json.loads(zlib.decompress(facts)),
        "manifest": {"source_key": inputs.source_key, "revision": inputs.revision,
                     "entries": [{"path": e.path, "oid": e.oid, "size": e.size, "owner": e.owner, "roles": ",".join(e.roles)} for e in entries]},
        "fingerprint": inputs.fingerprint,
        "local_sqlite_sha256": hashlib.sha256((fixtures / "legacy-v1-local.sqlite3").read_bytes()).hexdigest()}
samples = ["Catalog.X.Attribute.Y", "Catalog.X.StandardCommand.OpenByValue.Field", "Catalog.X.StandardCommand.Open", "Catalog.X.Item", "Catalog.X.EmptyRef", "cfg:CatalogRef.Номенклатура", "CatalogTabularSection.А.Строки.Колонка", "Characteristic.Виды", "style:Оформление", "Catalog.Unknown.One.Two.Three.Four.Five.Six", ".Catalog.X Catalog.Y", "myCatalog.X ЁCatalog.Y", "x:Catalog.X.Form.Основная", "Catalog.X.StandardAttribute.Ref", "Document.A;Catalog.B", "Catalog..X", "style:42_and_Ё", "Catalog.X.Attribute.Имя\nDocument.Y", "IntegrationService.X.Channel.Y", "ExternalDataSourceTable.X.Y", "ConstantsSet", "STYLE:Foo style:Ä😀"]
data["references"] = [{"text": text, "refs": [{field: getattr(ref, field) for field in ("kind", "name", "member", "member_name", "predefined")} for ref in helper["scan"](text)]} for text in samples]
data["stable_uuid"] = [{"seed": seed, "uuid": helper["stable_uuid"](seed)} for seed in ("11111111-1111-4111-8111-111111111111/partial-cf/_Перенос", "ASCII", "Ё😀")]
queries = [
    "ВЫБРАТЬ П.Ссылка, П.Менеджер.Наименование ИЗ Справочник.Партнеры КАК П",
    "SELECT I.Ref, I.Owner.Name FROM Catalog.Items AS I",
    "ВЫБРАТЬ Количество ИЗ РегистрНакопления.Остатки.Остатки(&Дата, ) КАК О",
    "ВЫБРАТЬ Т.* ИЗ РегистрСведений.Настройки КАК Т",
    "ВЫБРАТЬ Т.Номенклатура ИЗ Документ.Заказ.Товары Т",
    "ВЫБРАТЬ ЗНАЧЕНИЕ(Справочник.Валюты.Рубль) КАК Валюта",
    "SELECT VALUE(Enum.Kind.First) AS Kind",
    "SELECT VALUE(Catalog.Items.EmptyRef) AS Item",
    "ВЫБРАТЬ ВЫРАЗИТЬ(Д.Основание КАК Документ.Заказ).Склад КАК Склад ИЗ Документ.Продажа Д",
    "ВЫБРАТЬ Т.Объект ССЫЛКА Документ.Заказ, ТИП(Документ.Заказ) ИЗ Справочник.А Т",
    "ВЫБРАТЬ Задача.Исполнитель ИЗ Документ.Заказ КАК Задача",
    "ВЫБРАТЬ Т.Ссылка ИЗ Документ.А Т ОБЪЕДИНИТЬ ВСЕ ВЫБРАТЬ Т.Ссылка ИЗ Документ.Б Т",
    "ВЫБРАТЬ Вложенный.Ссылка.Поле ИЗ (ВЫБРАТЬ Д.Ссылка ИЗ Документ.А Д) Вложенный",
    "ВЫБРАТЬ ВЫБОР КОГДА ИСТИНА ТОГДА Д.Склад ИНАЧЕ ВЫРАЗИТЬ(Д.Основание КАК Документ.Заказ).Склад КОНЕЦ КАК Склад ИЗ Документ.Продажа Д",
    'SELECT "Catalog.NotSource.Field", &Catalog.Parameter // Document.Comment\n FROM Catalog.Real AS R',
    "ВЫБРАТЬ А.Поле ИЗ Справочник.А // comment\n КАК А",
    "ВЫБРАТЬ Т.СуммаОборотДт ИЗ РегистрБухгалтерии.Хозрасчетный.Обороты(&Начало, &Конец, , ) Т",
    "ВЫБРАТЬ ЗНАЧЕНИЕ(Справочник.А.Элемент) КАК Элемент; ВЫБРАТЬ Т.Элемент.Наименование ИЗ Справочник.Б Т",
    "ВЫБРАТЬ Р.Ссылка ИЗ РегистрСведений.А.СрезПоследних(, Код = &Код) КАК Р",
    "select R.Amount FROM AccumulationRegister.Stock.Balance() as R",
]
data["queries"] = []
for query in queries:
    fields = {}
    refs = helper["query_refs"](query, fields)
    data["queries"].append({"query": query, "refs": refs, "fields": fields})
modules = [
    'Запрос.Текст = "ВЫБРАТЬ ЗНАЧЕНИЕ(Перечисление.Статусы.ВРаботе)";',
    '// ЗНАЧЕНИЕ(Справочник.А.Элемент)\nЗНАЧЕНИЕ(Справочник.Б.Элемент);',
    'Строка = "http://host/ЗНАЧЕНИЕ(Справочник.А.Элемент)"; // comment',
    'Текст = "Первая строка\n|ЗНАЧЕНИЕ(Справочник.А.Элемент)"; // ЗНАЧЕНИЕ(Справочник.Б.Элемент)',
    'С = "двойные "" кавычки // внутри"; // снаружи\nVALUE(Enum.A.First)',
    'Объект.ЗНАЧЕНИЕ(Справочник.А.Элемент); VALUE ( Enum.A.First )',
    'ЗНАЧЕНИЕ(Справочник.А.ПустаяСсылка)\nVALUE(Catalog.A.EmptyRef)',
    'VALUE(Catalog.A.Item) VALUE(Catalog.A.Item) ЗНАЧЕНИЕ(Перечисление.Б.Второй)',
    'VALUE(Catalog.A) VALUE(Catalog.A.Item.Extra) VALUE(&Parameter)',
    '// только комментарий без перевода строки',
]
from cf_wizard.helper_worker import without_bsl_comments
data["modules"] = [{"text": text, "refs": helper["module_values"](text), "without_comments": without_bsl_comments(text)} for text in modules]
forms = [
    '<Form><Attributes><Attribute name="Объект"><Type><Type>cfg:DocumentObject.Заказ</Type></Type></Attribute></Attributes><ChildItems><InputField name="Склад"><DataPath>Объект.Склад.Наименование</DataPath></InputField></ChildItems></Form>',
    '<Form><Attributes><Attribute name="Таблица"><Type><Type>v8:ValueTable</Type></Type><Columns><Column name="Ссылка"><Type><Type>cfg:CatalogRef.А</Type><Type>cfg:DocumentRef.Б</Type></Type></Column></Columns></Attribute></Attributes><ChildItems><Table name="Список"><DataPath>Таблица</DataPath><Text>Items.Список.CurrentData.Ссылка.Наименование</Text></Table><InputField table="Таблица.Ссылка.Наименование" /></ChildItems></Form>',
    '<Form><Attributes><Attribute name="Список"><Type><Type>cfg:DynamicList</Type></Type><Settings><MainTable>Catalog.А</MainTable><QueryText>'+html.escape('ВЫБРАТЬ ЗНАЧЕНИЕ(Справочник.Б.Элемент) КАК Организация, Т.Склад КАК Склад ИЗ Документ.А Т')+'</QueryText></Settings></Attribute></Attributes><ChildItems><InputField name="ИНН"><DataPath>Список.Организация.ИНН</DataPath></InputField><InputField name="Тип"><DataPath>Список.Склад.Код</DataPath></InputField></ChildItems></Form>',
    '<Form><Attributes><Attribute name="Константы"><Type><Type>cfg:ConstantsSet</Type></Type></Attribute></Attributes><ChildItems><InputField name="Флаг"><DataPath>Константы.Использовать.Код</DataPath></InputField></ChildItems></Form>',
    '<Form><Attributes><Attribute name="Данные"><Type><Type>cfg:CatalogObject.Ёж</Type></Type></Attribute></Attributes><A>Данные.Код</A><B>Данные.Код</B><C>Items.Unknown.CurrentData.X</C></Form>',
]
data["forms"] = [{"xml": form, "paths": [[sorted(types), path] for types, path in helper["form_paths"](form)]} for form in forms]
data["stub_descriptors"] = []
for kind in ("Catalog", "InformationRegister", "AccumulationRegister", "FunctionalOption", "DocumentJournal", "Report", "DataProcessor"):
    key = f"{kind}.Тест"
    source = ('\ufeff<?xml version="1.0" encoding="UTF-8"?>\r\n'
        '<MetaDataObject xmlns:xr="http://v8.1c.ru/8.3/xcf/readable" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">\r\n'
        f'\t<{kind} uuid="11111111-1111-4111-8111-111111111111">\r\n'
        '\t\t<InternalInfo><GeneratedType>keep</GeneratedType></InternalInfo>\r\n\t\t<Properties>\r\n'
        '\t\t\t<Name>Тест</Name><Synonym/><Comment>Catalog.Комментарий</Comment><Type><Type>cfg:CatalogRef.Тип</Type></Type>\r\n'
        f'\t\t\t<DefaultForm>{key}.Form.Основная</DefaultForm><MainDataCompositionSchema>{key}.Template.Схема</MainDataCompositionSchema>\r\n'
        '\t\t\t<Owners>\r\n\t\t\t\t<xr:Item>Catalog.А</xr:Item>\r\n\t\t\t\t<xr:Item>Catalog.Б</xr:Item>\r\n\t\t\t</Owners>\r\n'
        '\t\t\t<BasedOn>\r\n\t\t\t\t<xr:Item>Document.Первый</xr:Item>\r\n\t\t\t\t<xr:Item>Document.Второй</xr:Item>\r\n\t\t\t</BasedOn>\r\n'
        '\t\t\t<RegisteredDocuments><xr:Item>Document.Первый</xr:Item></RegisteredDocuments>\r\n'
        '\t\t\t<Location>Catalog.Location.Attribute.Flag</Location>\r\n'
        '\t\t\t<Content><xr:Item>Catalog.А</xr:Item><xr:Item>Catalog.Б</xr:Item></Content>\r\n'
        '\t\t\t<ForeignNested><Container><xr:Item>Catalog.Другой</xr:Item></Container></ForeignNested>\r\n'
        f'\t\t\t<Self>{key}.StandardAttribute.Ref</Self>\r\n'
        '\t\t</Properties>\r\n\t\t<ChildObjects>\r\n'
        '\t\t\t<Attribute uuid="22222222-2222-4222-8222-222222222222"><Properties><Name>Реквизит</Name><Type><Type>cfg:CatalogRef.Другой</Type></Type></Properties></Attribute>\r\n'
        '\t\t\t<Dimension><Properties><Name>Измерение</Name><Master>true</Master></Properties></Dimension>\r\n'
        '\t\t\t<Resource><Properties><Name>Ресурс</Name></Properties></Resource>\r\n'
        '\t\t\t<Resource><Properties><Name>ВторойРесурс</Name></Properties></Resource>\r\n'
        '\t\t\t<Form>Основная</Form>\r\n\t\t\t<Template>Схема</Template>\r\n\t\t\t<Command>Команда</Command>\r\n'
        f'\t\t</ChildObjects>\r\n\t</{kind}>\r\n</MetaDataObject>')
    for members, predefined, based_on, by_value in (([],False,False,False),([("Attribute","Реквизит")],True,False,False),([("Form","Основная"),("Template","Схема"),("Command","Команда")],False,True,False),([("*","Измерение")],False,False,True),([("Resource","ВторойРесурс")],False,False,False)):
        for present in ([],["Catalog.А","Document.Второй"]):
            stub = helper["Stub"]()
            stub.members = set(members)
            stub.predefined, stub.based_on, stub.by_value = predefined, based_on, by_value
            keep = []
            helper["_children_cache"].clear()
            result = helper["stub_descriptor"](source,key,stub,set(present),keep)
            data["stub_descriptors"].append({"xml":source,"key":key,"spec":{"members":sorted(members),"predefined":predefined,"based_on":based_on,"by_value":by_value},"present":present,"output":result,"patterns":keep})
data["merge_settings"] = []
for configuration, selected, stubs, rules in (("Fixture",["Catalog.А","Document.Б"],["Catalog.В"],{"Catalog.В":["ObjectModule","ManagerModule"],"Catalog.В.Form.Основная":["Form"]}),("",["CommonModule.А"],[],{}),("Ёж",[],["Catalog.Флаг"],{"Catalog.Флаг":["Content"]})):
    result = helper["Result"]("_Перенос")
    result.configuration, result.selected = configuration, sorted(selected)
    result.stubs = {key:helper["Stub"]() for key in stubs}
    result.rules = rules
    data["merge_settings"].append({"configuration":configuration,"selected":sorted(selected),"stubs":sorted(stubs),"rules":rules,"xml":helper["merge_settings"](result)})
(fixtures / "oracle-0.7.3.json").write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print("Pinned 0.7.3 oracle:", inputs.fingerprint)
