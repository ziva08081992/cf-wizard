"""Build the Russian DOCX guide from docs/user-guide.md. Requires python-docx."""
from pathlib import Path
import argparse
import re
from urllib.parse import quote, urlsplit
from docx import Document
from docx.enum.table import WD_TABLE_ALIGNMENT, WD_CELL_VERTICAL_ALIGNMENT
from docx.enum.text import WD_ALIGN_PARAGRAPH
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.opc.constants import RELATIONSHIP_TYPE as RT
from docx.shared import Inches, Pt, RGBColor

ROOT=Path(__file__).resolve().parents[2]
SOURCE=ROOT/'docs/user-guide.md'
VERSION='0.8.0-rust.2'
OUTPUT=ROOT/f'docs/CF-Wizard-{VERSION}-User-Guide-RU-Illustrated.docx'

def link_target(target):
    if urlsplit(target).scheme or target.startswith('#'):
        return target
    relative=(SOURCE.parent/target.split('?')[0]).resolve().relative_to(ROOT).as_posix()
    query='?'+target.split('?',1)[1] if '?' in target else ''
    return 'https://github.com/ziva08081992/cf-wizard/blob/main/'+quote(relative)+query

def rich(paragraph,text):
    token=re.compile(r'(\[([^\]]+)\]\(([^)]+)\)|`([^`]+)`|\*\*([^*]+)\*\*)')
    start=0
    for match in token.finditer(text):
        paragraph.add_run(text[start:match.start()])
        if match.group(2):
            link=OxmlElement('w:hyperlink');link.set(qn('r:id'),paragraph.part.relate_to(link_target(match.group(3)),RT.HYPERLINK,is_external=True))
            run=OxmlElement('w:r');pr=OxmlElement('w:rPr');color=OxmlElement('w:color');color.set(qn('w:val'),'18588A');pr.append(color);run.append(pr)
            t=OxmlElement('w:t');t.text=match.group(2);run.append(t);link.append(run);paragraph._p.append(link)
        else:
            run=paragraph.add_run(match.group(4) or match.group(5));run.bold=bool(match.group(5))
            if match.group(4):run.font.name='Consolas';run.font.size=Pt(10)
        start=match.end()
    paragraph.add_run(text[start:])

def numbering(document):
    tree=document.part.numbering_part.element
    abstracts=tree.findall(qn('w:abstractNum'))
    abstract=next((a for a in abstracts if any(x.get(qn('w:val'))=='ListNumber' for x in a.iter(qn('w:pStyle')))),abstracts[0])
    ids=[int(n.get(qn('w:numId'))) for n in tree.findall(qn('w:num'))]
    ident=max(ids,default=0)+1
    num=OxmlElement('w:num');num.set(qn('w:numId'),str(ident));ref=OxmlElement('w:abstractNumId');ref.set(qn('w:val'),abstract.get(qn('w:abstractNumId')));num.append(ref)
    override=OxmlElement('w:lvlOverride');override.set(qn('w:ilvl'),'0');start=OxmlElement('w:startOverride');start.set(qn('w:val'),'1');override.append(start);num.append(override);tree.append(num)
    return ident

def table(document,lines):
    rows=[[cell.strip() for cell in row.strip().strip('|').split('|')] for row in lines]
    rows=[rows[0]]+rows[2:]
    t=document.add_table(rows=0,cols=len(rows[0]));t.autofit=False;t.alignment=WD_TABLE_ALIGNMENT.CENTER
    t.columns[0].width=Inches(2.0);t.columns[1].width=Inches(5.0)
    for index,values in enumerate(rows):
        row=t.add_row();pr=row._tr.get_or_add_trPr();pr.append(OxmlElement('w:cantSplit'))
        if index==0:pr.append(OxmlElement('w:tblHeader'))
        for col,value in enumerate(values):
            cell=row.cells[col];cell.width=Inches(2.0 if col==0 else 5.0);cell.vertical_alignment=WD_CELL_VERTICAL_ALIGNMENT.CENTER
            tcpr=cell._tc.get_or_add_tcPr();borders=OxmlElement('w:tcBorders')
            for edge in ('top','left','bottom','right','insideH','insideV'):
                b=OxmlElement('w:'+edge);b.set(qn('w:val'),'single');b.set(qn('w:sz'),'4');b.set(qn('w:color'),'D9D9D9');borders.append(b)
            tcpr.append(borders);margins=OxmlElement('w:tcMar')
            for edge in ('top','left','bottom','right'):
                m=OxmlElement('w:'+edge);m.set(qn('w:w'),'95');m.set(qn('w:type'),'dxa');margins.append(m)
            tcpr.append(margins);shade=OxmlElement('w:shd');shade.set(qn('w:fill'),'243746' if index==0 else ('F2F5F7' if index%2==0 else 'FFFFFF'));tcpr.append(shade)
            p=cell.paragraphs[0];p.paragraph_format.space_after=Pt(2);p.paragraph_format.space_before=Pt(2);rich(p,value)
            for r in p.runs:r.font.size=Pt(10.5);r.bold=index==0;r.font.color.rgb=RGBColor.from_string('FFFFFF' if index==0 else '000000')
    document.add_paragraph().paragraph_format.space_after=Pt(2)

def build():
    doc=Document();section=doc.sections[0];section.page_width=Inches(8.5);section.page_height=Inches(11)
    section.top_margin=section.bottom_margin=Inches(.7);section.left_margin=section.right_margin=Inches(.75)
    for name in ('Normal','Title','Subtitle','Heading 1','Heading 2','Heading 3','List Number','List Bullet','Caption'):
        style=doc.styles[name];style.font.name='Calibri';style.font.color.rgb=RGBColor(0,0,0)
        style.element.get_or_add_rPr().get_or_add_rFonts().set(qn('w:ascii'),'Calibri');style.element.rPr.rFonts.set(qn('w:hAnsi'),'Calibri')
        style.font.size=Pt(11);style.paragraph_format.space_after=Pt(6);style.paragraph_format.line_spacing=1.1
        ppr=style.element.get_or_add_pPr()
        for border in list(ppr.findall(qn('w:pBdr'))):ppr.remove(border)
    doc.styles['Title'].font.size=Pt(23);doc.styles['Title'].paragraph_format.space_after=Pt(8)
    for name,size in (('Heading 1',16),('Heading 2',12)):
        doc.styles[name].font.size=Pt(size);doc.styles[name].font.bold=True;doc.styles[name].paragraph_format.space_before=Pt(13);doc.styles[name].paragraph_format.keep_with_next=True
    doc.styles['Caption'].font.size=Pt(9.5);doc.styles['Caption'].font.color.rgb=RGBColor.from_string('4A4A4A')
    footer=section.footer.paragraphs[0];footer.alignment=WD_ALIGN_PARAGRAPH.CENTER
    r=footer.add_run(f'CF Wizard {VERSION}   |   ');r.font.size=Pt(9)
    field=OxmlElement('w:fldSimple');field.set(qn('w:instr'),'PAGE');footer._p.append(field)
    lines=SOURCE.read_text(encoding='utf-8-sig').splitlines();i=0;num=None
    while i<len(lines):
        line=lines[i].strip()
        if not line:i+=1;num=None;continue
        if line=='<!-- pagebreak -->':
            doc.add_page_break();i+=1;num=None;continue
        if line.startswith('|'):
            group=[]
            while i<len(lines) and lines[i].strip().startswith('|'):group.append(lines[i]);i+=1
            table(doc,group);continue
        img=re.fullmatch(r'!\[([^\]]+)\]\(([^)]+)\)',line)
        if img:
            p=doc.add_paragraph();p.alignment=WD_ALIGN_PARAGRAPH.CENTER;p.paragraph_format.keep_with_next=True
            shape=p.add_run().add_picture(str(SOURCE.parent/img.group(2)),width=Inches(6.8));shape._inline.docPr.set('descr',img.group(1))
            cap=doc.add_paragraph(img.group(1),style='Caption');cap.alignment=WD_ALIGN_PARAGRAPH.CENTER
        elif line.startswith('# '):doc.add_paragraph(line[2:],style='Title')
        elif line.startswith('## '):doc.add_paragraph(line[3:],style='Heading 1')
        elif line.startswith('### '):doc.add_paragraph(line[4:],style='Heading 2')
        elif re.match(r'^\d+\. ',line):
            if num is None:num=numbering(doc)
            p=doc.add_paragraph(style='List Number');rich(p,re.sub(r'^\d+\. ','',line))
            props=p._p.get_or_add_pPr();np=OxmlElement('w:numPr');ni=OxmlElement('w:numId');ni.set(qn('w:val'),str(num));np.append(ni);props.append(np)
        elif line.startswith('- '):rich(doc.add_paragraph(style='List Bullet'),line[2:])
        else:rich(doc.add_paragraph(),line)
        i+=1
    doc.core_properties.title='CF Wizard Руководство пользователя';doc.core_properties.subject=f'Использование версии {VERSION}';doc.core_properties.author='CF Wizard';doc.core_properties.last_modified_by='CF Wizard';doc.core_properties.keywords='1С CF Git Gitea GitHub инструкция'
    doc.core_properties.language='ru-RU'
    OUTPUT.parent.mkdir(parents=True,exist_ok=True)
    doc.save(OUTPUT)
    print(OUTPUT);print('Paragraphs:',len(doc.paragraphs),'Tables:',len(doc.tables),'Images:',len(doc.inline_shapes))
if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source',type=Path,default=SOURCE)
    parser.add_argument('--output',type=Path,default=OUTPUT)
    parser.add_argument('--version',default=VERSION)
    options=parser.parse_args()
    SOURCE=options.source.resolve();OUTPUT=options.output.resolve();VERSION=options.version
    build()
