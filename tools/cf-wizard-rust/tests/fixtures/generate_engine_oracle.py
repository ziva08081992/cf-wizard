"""Independent end-to-end 0.7.3 oracle. Development only, never part of runtime."""
from pathlib import Path
import base64
import hashlib
import json
import os
import runpy
import sqlite3
import sys
import tempfile
from contextlib import closing

fixtures = Path(__file__).resolve().parent
repo = fixtures.parents[3]
project = repo.parents[2]
sys.path.insert(0, str(repo / "tools/1c-delivery"))
from cf_wizard.helper_worker import resolve_dependencies, HELPER_SHA256
from cf_wizard.dependency_index import update_index
from cf_wizard.git_index_inputs import GitIndexInputs, GitIndexEntry
from cf_wizard.git_index_store import INPUT_SCHEMA

helper_path = project / ".agents/skills/1c-partial-cf/scripts/partial_cf.py"
assert hashlib.sha256(helper_path.read_bytes()).hexdigest() == HELPER_SHA256
h = runpy.run_path(str(helper_path), run_name="engine_oracle")
header = ('\ufeff<?xml version="1.0" encoding="UTF-8"?>\r\n'
    '<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" '
    'xmlns:cfg="http://v8.1c.ru/8.1/data/enterprise/current-config" '
    'xmlns:xr="http://v8.1c.ru/8.3/xcf/readable" '
    'xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">\r\n\t')

def obj(key, props="", children=""):
    kind, name = key.split(".", 1)
    uid = h["stable_uuid"]("oracle/" + key)
    return (header + f'<{kind} uuid="{uid}">\r\n\t\t<Properties><Name>{name}</Name>'
        f'<Synonym/><Comment/>{props}</Properties>\r\n\t\t<ChildObjects>{children}</ChildObjects>\r\n'
        f'\t</{kind}>\r\n</MetaDataObject>').encode("utf-8")

def member(tag, name, kinds=(), extra="", children=""):
    types = "<Type>" + "".join(f"<Type>cfg:{kind}</Type>" for kind in kinds) + "</Type>" if kinds else ""
    cols = f"<ChildObjects>{children}</ChildObjects>" if children else ""
    return f"<{tag}><Properties><Name>{name}</Name>{types}{extra}</Properties>{cols}</{tag}>"

def cfg(keys):
    children = "\r\n\t\t\t<Language>Русский</Language>" + "".join(
        f"\r\n\t\t\t<{key.split('.')[0]}>{key.split('.')[1]}</{key.split('.')[0]}>" for key in sorted(keys))
    return (header + '<Configuration uuid="11111111-1111-4111-8111-111111111111">\r\n'
        '\t\t<Properties><Name>Fixture</Name><Synonym/><Comment/>'
        '<DefaultLanguage>Language.Русский</DefaultLanguage>'
        '<DefaultRoles><xr:Item>Role.Unselected</xr:Item></DefaultRoles></Properties>\r\n'
        f'\t\t<ChildObjects>{children}\r\n\t\t</ChildObjects>\r\n\t</Configuration>\r\n</MetaDataObject>').encode("utf-8")

def source(objects, extras):
    files = {h["descriptor_rel"](key): value for key, value in objects.items()}
    files["Configuration.xml"] = cfg(objects)
    files["Languages/Русский.xml"] = obj("Language.Русский")
    files.update({path: value.encode("utf-8") if isinstance(value,str) else value for path,value in extras.items()})
    return files

def form(types, path):
    return ('<Form><Attributes><Attribute name="Объект"><Type>'
        + "".join(f"<Type>cfg:{kind}</Type>" for kind in types)
        + '</Type></Attribute></Attributes><ChildItems><InputField name="Поле"><DataPath>'
        + path + '</DataPath></InputField></ChildItems></Form>')

cases = []
basic = {
    "CommonModule.А": obj("CommonModule.А"),
    "Enum.Статусы": obj("Enum.Статусы",children=member("EnumValue","Первый")+member("EnumValue","Второй")),
    "Catalog.Тест": obj("Catalog.Тест","<CodeLength>0</CodeLength><CodeType>String</CodeType>"),
}
predefined = '<PredefinedData><Item><Name>Элемент</Name><Code>001</Code></Item></PredefinedData>'
for label, module, excluded in (
    ("required-values",'X = "VALUE(Enum.Статусы.Первый) ЗНАЧЕНИЕ(Справочник.Тест.Элемент)";',[]),
    ("comment-values",'// ЗНАЧЕНИЕ(Справочник.Тест.Элемент)\nX = "VALUE(Enum.Статусы.Первый)";',[]),
    ("excluded-comment",'// ЗНАЧЕНИЕ(Справочник.Тест.Элемент)\nX = "VALUE(Enum.Статусы.Первый)";',["Catalog.Тест"]),
    ("not-a-general-callgraph","Catalogs.Тест.GetRef(); // обычный вызов не является VALUE",[]),
):
    files = source(basic,{"CommonModules/А/Ext/Module.bsl":module,"Catalogs/Тест/Ext/Predefined.xml":predefined})
    cases.append((label,files,["CommonModule.А"],excluded))

composite = {
    "Document.Продажа": obj("Document.Продажа",children=member("Attribute","Объект",["DefinedType.Составной"])+"<Form>Основная</Form>"),
    "DefinedType.Составной": obj("DefinedType.Составной","<Type><Type>cfg:CatalogRef.Счет</Type><Type>cfg:CatalogRef.Валюта</Type></Type>"),
    "Catalog.Счет": obj("Catalog.Счет",children=member("Attribute","НужноеПоле")+member("Attribute","Лишнее")),
    "Catalog.Валюта": obj("Catalog.Валюта",children=member("Attribute","НужноеПоле")+member("Attribute","Лишнее")),
}
cases.append(("all-composite-types",source(composite,{
    "Documents/Продажа/Forms/Основная.xml":obj("CommonForm.Основная"),
    "Documents/Продажа/Forms/Основная/Ext/Form.xml":form(["DocumentObject.Продажа"],"Объект.Объект.НужноеПоле"),
    "Documents/Продажа/Forms/Основная/Ext/Form/Module.bsl":"// keep selected module exactly\r\n",
    "Documents/Продажа/Templates/Данные/Ext/Template.bin":b"\x00\xffCatalog.NotAReference",
}),["Document.Продажа"],[]))

registers = {
    "CommonForm.Список": obj("CommonForm.Список"),
    "AccumulationRegister.Остатки":obj("AccumulationRegister.Остатки",children=
        member("Dimension","Склад",["CatalogRef.Склады"])+member("Resource","Количество")+member("Resource","Сумма")),
    "Document.Альфа":obj("Document.Альфа","<RegisterRecords><xr:Item>AccumulationRegister.Остатки</xr:Item></RegisterRecords>"),
    "Document.Ясный":obj("Document.Ясный","<RegisterRecords><xr:Item>AccumulationRegister.Остатки</xr:Item></RegisterRecords>",
        member("Attribute","Склад",["CatalogRef.Склады"])),
    "Catalog.Склады":obj("Catalog.Склады",children=member("Attribute","НужноеПоле")+member("Attribute","Лишнее")),
    "Catalog.Организации":obj("Catalog.Организации",children=member("Attribute","ИНН")),
    "CommonAttribute.Организация":obj("CommonAttribute.Организация",
        "<Type><Type>cfg:CatalogRef.Организации</Type></Type><AutoUse>Use</AutoUse>"
        "<Content><xr:Item><xr:Metadata>Catalog.Склады</xr:Metadata><xr:Use>Use</xr:Use></xr:Item></Content>"),
}
for label, path, selection in (
    ("register-resource-and-first-registrar","Объект.КоличествоОстаток",["CommonForm.Список"]),
    ("register-recorder-and-common-attribute","Объект.Регистратор.Склад.Организация.ИНН",["CommonForm.Список"]),
    ("prefer-included-recorder","Объект.Регистратор.Склад.НужноеПоле",["CommonForm.Список","Document.Ясный"]),
):
    cases.append((label,source(registers,{"CommonForms/Список/Ext/Form.xml":form(["AccumulationRegisterRecordSet.Остатки"],path)}),selection,[]))

opened = {
    "CommonForm.Пуск":obj("CommonForm.Пуск"),
    "Report.Отчет":obj("Report.Отчет","<DefaultForm>Report.Отчет.Form.Основная</DefaultForm>"
        "<MainDataCompositionSchema>Report.Отчет.Template.Схема</MainDataCompositionSchema>",
        "<Form>Основная</Form><Form>Лишняя</Form><Template>Схема</Template><Command>Команда</Command>"),
    "Catalog.Данные":obj("Catalog.Данные"),
    "Catalog.Скрытый":obj("Catalog.Скрытый"),
    "Document.Основание":obj("Document.Основание"),
    "DataProcessor.Обработка":obj("DataProcessor.Обработка","<BasedOn><xr:Item>Document.Основание</xr:Item></BasedOn>"),
    "InformationRegister.Ключи":obj("InformationRegister.Ключи",children=
        member("Dimension","Первое",extra="<Master>true</Master>")+member("Dimension","Второе",extra="<Master>true</Master>")+
        member("Dimension","НеКлюч")+member("Resource","Данные")),
}
cases.append(("standard-commands-and-kept-modules",source(opened,{
    "CommonForms/Пуск/Ext/Form.xml":"<Form><Actions>Report.Отчет.StandardCommand.Open "
        "Report.Отчет.Command.Команда DataProcessor.Обработка.StandardCommand.CreateBasedOn "
        "InformationRegister.Ключи.StandardCommand.OpenByValue.Первое</Actions></Form>",
    "Reports/Отчет/Forms/Основная.xml":obj("CommonForm.Основная"),
    "Reports/Отчет/Forms/Основная/Ext/Form.xml":"<Form><Text>Catalog.Данные</Text></Form>",
    "Reports/Отчет/Forms/Основная/Ext/Form/Module.bsl":'X = "VALUE(Catalog.Скрытый.Элемент)";',
    "Reports/Отчет/Templates/Схема.xml":obj("CommonTemplate.Схема"),
    "Reports/Отчет/Templates/Схема/Ext/Template.bin":b"\x00\xab\xffschema",
    "Reports/Отчет/Commands/Команда/Ext/CommandModule.bsl":'X = "VALUE(Catalog.Скрытый.Элемент)";',
    "Reports/Отчет/Commands/Команда/Ext/Command.xml":"<Command><Data>Catalog.Данные</Data></Command>",
    "Reports/Отчет/Ext/ManagerModule.bsl":"Процедура Тест()\r\nКонецПроцедуры",
}),["CommonForm.Пуск"],[]))

paths = {
    "CommonForm.Путь":obj("CommonForm.Путь"),
    "Catalog.Родитель":obj("Catalog.Родитель","<Owners><xr:Item>Catalog.Владелец</xr:Item></Owners>",
        member("TabularSection","Строки",children=member("Attribute","Данные",["CatalogRef.Дочерний"]))),
    "Catalog.Владелец":obj("Catalog.Владелец",children=member("Attribute","Нужное")),
    "Catalog.Дочерний":obj("Catalog.Дочерний",children=member("Attribute","Нужное")),
    "Constant.Настройка":obj("Constant.Настройка","<Type><Type>cfg:CatalogRef.Родитель</Type></Type>"),
}
for label, kinds, path in (
    ("catalog-owner",["CatalogObject.Родитель"],"Объект.Владелец.Нужное"),
    ("tabular-section-type",["CatalogObject.Родитель"],"Объект.Строки.Данные.Нужное"),
    ("tabular-section-ref",["CatalogObject.Родитель"],"Объект.Строки.Ссылка.Владелец.Нужное"),
    ("constants-set",["ConstantsSet"],"Объект.Настройка.Владелец.Нужное"),
):
    cases.append((label,source(paths,{"CommonForms/Путь/Ext/Form.xml":form(kinds,path)}),["CommonForm.Путь"],[]))

cycles = {
    "CommonForm.Цикл":obj("CommonForm.Цикл"),
    "Catalog.А":obj("Catalog.А",children=member("Attribute","Сосед",["CatalogRef.Б"])+member("Attribute","Нужное")),
    "Catalog.Б":obj("Catalog.Б",children=member("Attribute","Сосед",["CatalogRef.А"])+member("Attribute","Нужное")),
}
cases.append(("cyclic-deep-path",source(cycles,{"CommonForms/Цикл/Ext/Form.xml":form(["CatalogObject.А"],"Объект."+"Сосед."*12+"Нужное")}),["CommonForm.Цикл"],[]))
wild = '<Form><Attributes><Attribute name="Список"><Type><Type>cfg:DynamicList</Type></Type><Settings><QueryText>SELECT A.* FROM Catalog.Родитель AS A</QueryText></Settings></Attribute></Attributes></Form>'
cases.append(("query-wildcard-all-members",source(paths,{"CommonForms/Путь/Ext/Form.xml":wild}),["CommonForm.Путь"],[]))
preferred = dict(registers)
preferred["Document.Альфа"] = obj("Document.Альфа","<RegisterRecords><xr:Item>AccumulationRegister.Остатки</xr:Item></RegisterRecords>",member("Attribute","Склад",["CatalogRef.Склады"]))
cases.append(("prefer-included-recorder-both-match",source(preferred,{"CommonForms/Список/Ext/Form.xml":form(["AccumulationRegisterRecordSet.Остатки"],"Объект.Регистратор.Склад.НужноеПоле")}),["CommonForm.Список","Document.Ясный"],[]))

def b64(value):
    return base64.b64encode(value).decode("ascii")
def blob(value):
    return hashlib.sha1(b"blob "+str(len(value)).encode()+b"\0"+value).hexdigest()

rows=[]
for label,files,selected,excluded in cases:
    with tempfile.TemporaryDirectory(prefix="cf-rust-oracle-") as temp:
        parent=Path(temp); src=parent/"source"; src.mkdir()
        assert parent.resolve().parent == Path(tempfile.gettempdir()).resolve() and parent.name.startswith("cf-rust-oracle-")
        for relative,data in files.items():
            path=src/relative; path.parent.mkdir(parents=True,exist_ok=True); path.write_bytes(data)
        plan=resolve_dependencies(helper_path,src,tuple(sorted(selected)),None,os.path.normcase(str(src.resolve())),frozenset(excluded))
        result=plan.writer.gi_frame.f_locals["res"]
        expected_stubs={}
        for row in plan.result["stubs"]:
            key=row["object"]; owners=plan.result["causes"][key]; primary=row["referred_by"]
            expected_stubs[key]={"key":key,"causes":[{"owner":owner,"required":owners[owner]} for owner in [primary,*sorted(set(owners)-{primary})]]}
        members={key:sorted(tag+"."+name for tag,name in stub.members) for key,stub in result.stubs.items()}
        stage=parent/"stage"; settings=parent/"settings.xml"
        plan.write(stage,settings)
        inventory={str(path.relative_to(stage)).replace("\\","/"):b64(path.read_bytes()) for path in sorted(stage.rglob("*")) if path.is_file()}
        h["_children_cache"].clear()
        descriptors={}
        # Use every declared top-level descriptor, including Language, for the old index.
        import xml.etree.ElementTree as ET
        rootxml=ET.fromstring(files["Configuration.xml"])
        for node in rootxml.find("{*}Configuration/{*}ChildObjects"):
            kind=node.tag.rsplit("}",1)[-1]; key=kind+"."+node.text
            descriptors[key]=h["descriptor_rel"](key)
        database=parent/"legacy.sqlite3"
        update_index(database,src,os.path.normcase(str(src.resolve())),descriptors,h,HELPER_SHA256,lambda:None)
        entries=[]
        for key,path in descriptors.items():
            roles=("descriptor","registrar") if key.startswith("Document.") else ("descriptor",)
            entries.append(GitIndexEntry(path,blob(files[path]),len(files[path]),key,roles))
        entries.append(GitIndexEntry("Configuration.xml",blob(files["Configuration.xml"]),len(files["Configuration.xml"]),"",("configuration",)))
        inputs=GitIndexInputs("git:oracle:"+label,"a"*40,tuple(entries))
        with closing(sqlite3.connect(database)) as db, db:
            db.executescript(INPUT_SCHEMA)
            db.executemany("INSERT INTO git_inputs VALUES (?,?,?,?,?)",(entry.row() for entry in inputs.entries))
            db.executemany("INSERT OR REPLACE INTO metadata VALUES (?,?)",{**inputs.context,"indexed_revision":inputs.revision}.items())
        rows.append({"name":label,"source":{path:b64(data) for path,data in files.items()},"selected":sorted(selected),"excluded":excluded,
            "stubs":expected_stubs,"members":members,"warnings":plan.result["predefined_code_mismatch"],"stage":inventory,"settings":b64(settings.read_bytes()),
            "legacy_index":b64(database.read_bytes()),"manifest":{"source_key":inputs.source_key,"revision":inputs.revision,
            "entries":[{"path":e.path,"oid":e.oid,"size":e.size,"owner":e.owner,"roles":",".join(e.roles)} for e in inputs.entries]}})
        plan.close()
(fixtures/"engine-0.7.3.json").write_text(json.dumps({"helper_sha256":HELPER_SHA256,"cases":rows},ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
print("Pinned complete-engine oracle cases:",len(rows))
