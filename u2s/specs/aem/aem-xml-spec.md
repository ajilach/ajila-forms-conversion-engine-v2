# AEM Adaptive Forms Content XML Specification (Best-Effort)

> **Status:** Unofficial, reverse-engineered specification.
> Adobe does not publish a formal DTD/XSD for AEM Adaptive Forms content XML.
> This document was assembled from:
>
> - Apache Jackrabbit FileVault documentation
> - The open-source [`aem-core-forms-components`](https://github.com/adobe/aem-core-forms-components) repository
> - Adobe Experience League documentation
> - Real exported AEM Forms content packages (Foundation Components)
> - The Blueprint project's own AEM XML writer (`src/aem/`)

---

## Table of Contents

1. [Overview](#1-overview)
2. [Package Structure (FileVault)](#2-package-structure-filevault)
3. [XML Namespace Declarations](#3-xml-namespace-declarations)
4. [JCR Node Type Definitions](#4-jcr-node-type-definitions)
5. [Form Page Content XML](#5-form-page-content-xml)
6. [Component Reference](#6-component-reference)
7. [Layout & Responsive Grid](#7-layout--responsive-grid)
8. [Rules & Scripts](#8-rules--scripts)
9. [DAM Asset Content XML](#9-dam-asset-content-xml)
10. [Translation Dictionaries](#10-translation-dictionaries)
11. [Intermediate Folder Content XML](#11-intermediate-folder-content-xml)
12. [META-INF / Vault Metadata](#12-meta-inf--vault-metadata)
13. [Attribute Value Type Hints](#13-attribute-value-type-hints)
14. [Component Families](#14-component-families)
15. [Expressions & Scripting Model](#15-expressions--scripting-model)
16. [Submit Actions](#16-submit-actions)
17. [Prefill Data Structure](#17-prefill-data-structure)
18. [Picture Clause Patterns](#18-picture-clause-patterns)
19. [Form Data Model Binding](#19-form-data-model-binding)

---

## 1. Overview

AEM Adaptive Forms are stored as JCR (Java Content Repository) node trees. When exported, each node tree is serialized into `.content.xml` files following the **Apache Jackrabbit FileVault** serialization format. A complete form consists of two parallel JCR subtrees:

| Tree | JCR Path | Purpose |
|------|----------|---------|
| **Form page** | `/content/forms/af/<path>/<form-code>` | The actual form definition (panels, fields, layout) |
| **DAM asset** | `/content/dam/formsanddocuments/<path>/<form-code>` | Metadata entry visible in the Forms Manager UI |

Both trees are bundled into a single FileVault content package (ZIP file).

---

## 2. Package Structure (FileVault)

A content package is a ZIP archive with the following layout:

```
<package>.zip
├── META-INF/
│   ├── MANIFEST.MF
│   └── vault/
│       ├── config.xml
│       ├── filter.xml
│       ├── nodetypes.cnd
│       ├── properties.xml
│       └── definition/
│           └── .content.xml
└── jcr_root/
    ├── .content.xml                          # rep:root
    └── content/
        ├── .content.xml                      # sling:OrderedFolder
        ├── forms/
        │   ├── .content.xml                  # sling:OrderedFolder
        │   └── af/
        │       ├── .content.xml              # sling:Folder
        │       └── <path-segments>/          # sling:OrderedFolder (each)
        │           └── <form-code>/
        │               ├── .content.xml      # cq:Page (FORM CONTENT)
        │               └── _jcr_content/
        │                   └── guideContainer/
        │                       └── assets/
        │                           └── dictionary/
        │                               ├── de.xml   # translation files
        │                               ├── fr.xml
        │                               └── ...
        └── dam/
            ├── .content.xml                  # sling:Folder
            └── formsanddocuments/
                ├── .content.xml              # sling:Folder
                └── <path-segments>/          # sling:OrderedFolder (each)
                    └── <form-code>/
                        └── .content.xml      # dam:Asset (DAM METADATA)
```

**Key conventions:**
- JCR node names containing `:` are escaped in the filesystem: `jcr:content` → `_jcr_content`
- Each directory can have a `.content.xml` that defines the JCR properties for that node
- Translation dictionaries are stored as individual XML files (not `.content.xml`) under the `dictionary/` folder, one per locale

---

## 3. XML Namespace Declarations

All `.content.xml` files declare XML namespaces via `xmlns:` attributes on the root element. The standard namespaces are:

| Prefix | URI | Usage |
|--------|-----|-------|
| `jcr` | `http://www.jcp.org/jcr/1.0` | JCR standard properties (`jcr:primaryType`, `jcr:title`, etc.) |
| `sling` | `http://sling.apache.org/jcr/sling/1.0` | Apache Sling properties (`sling:resourceType`, `sling:Folder`) |
| `cq` | `http://www.day.com/jcr/cq/1.0` | Adobe CQ/AEM properties (`cq:Page`, `cq:template`) |
| `nt` | `http://www.jcp.org/jcr/nt/1.0` | JCR node types (`nt:unstructured`, `nt:folder`) |
| `fd` | `http://www.adobe.com/aemfd/fd/1.0` | AEM Forms Designer namespace (`fd:rules`, `fd:version`) |
| `dam` | `http://www.day.com/dam/1.0` | DAM asset types (`dam:Asset`, `dam:AssetContent`) |
| `mix` | `http://www.jcp.org/jcr/mix/1.0` | JCR mixin types (`mix:language`, `mix:created`) |
| `rep` | `internal` | Jackrabbit internal (`rep:root`, `rep:AccessControllable`) |
| `vlt` | `http://www.day.com/jcr/vault/1.0` | FileVault package definition (`vlt:PackageDefinition`) |
| `xmp` | `http://ns.adobe.com/xap/1.0/` | XMP metadata (sometimes on DAM assets) |

A typical form `.content.xml` declares:

```xml
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:fd="http://www.adobe.com/aemfd/fd/1.0"
          xmlns:cq="http://www.day.com/jcr/cq/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:nt="http://www.jcp.org/jcr/nt/1.0"
          jcr:primaryType="cq:Page">
```

Only namespaces actually used in the document need to be declared; the set varies per file.

---

## 4. JCR Node Type Definitions

The node types used in AEM Forms packages are defined in `META-INF/vault/nodetypes.cnd` using CND (Compact Node Type Definition) notation. Key types:

### Primary Node Types

| Node Type | Parent Type | Description |
|-----------|-------------|-------------|
| `cq:Page` | `nt:hierarchyNode` | A content page; orderable, primary item is `jcr:content` |
| `cq:PageContent` | `nt:unstructured` | Page content node; mixes in `cq:OwnerTaggable`, `cq:ReplicationStatus`, `mix:created`, `mix:title`, `sling:Resource`, `sling:VanityPath`; orderable |
| `nt:unstructured` | — | Generic unstructured node; used for most form components |
| `sling:Folder` | `nt:folder` | Sling-aware folder |
| `sling:OrderedFolder` | `sling:Folder` | Ordered folder (child order is preserved) |
| `dam:Asset` | `nt:hierarchyNode` | DAM asset; primary item is `jcr:content` |
| `dam:AssetContent` | `nt:unstructured` | DAM asset content; has `metadata`, `related`, `renditions` children |
| `vlt:PackageDefinition` | — | FileVault package definition (in `META-INF/vault/definition/`) |
| `rep:root` | — | Repository root node |

### Mixin Types

| Mixin Type | Description |
|------------|-------------|
| `sling:Resource` | Adds `sling:resourceType` property |
| `sling:Message` | I18n message; adds `sling:key` (String) and `sling:message` (undefined) |
| `mix:language` | Adds `jcr:language` property |
| `mix:created` | Adds `jcr:created` / `jcr:createdBy` |
| `mix:title` | Adds `jcr:title` / `jcr:description` |
| `cq:Taggable` | Adds `cq:tags` (String[]) |
| `rep:AccessControllable` | Adds access-control policy support |
| `fd:xdp` | Adds `fd:trusted` (Boolean) |

---

## 5. Form Page Content XML

The form page `.content.xml` is the main form definition. It has a fixed hierarchical structure:

```
jcr:root                    (cq:Page)
└── jcr:content             (cq:PageContent)
    └── guideContainer      (nt:unstructured, guideContainerNode)
        └── rootPanel       (nt:unstructured, guideRootPanel)
            ├── layout      (nt:unstructured)
            ├── items       (nt:unstructured)
            │   ├── panel_<uuid>     (guidePanel)
            │   │   ├── items
            │   │   │   ├── textbox_<uuid>     (guideTextBox)
            │   │   │   ├── checkbox_<uuid>    (guideCheckBox)
            │   │   │   └── ...
            │   │   └── layout
            │   └── ...
            └── toolbar     (guideToolbar)
                ├── previtemnav   (guideButton)
                ├── nextitemnav   (guideButton)
                └── submit        (guideButton)
```

### 5.1. `jcr:root` (Page Node)

```xml
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:fd="http://www.adobe.com/aemfd/fd/1.0"
          xmlns:cq="http://www.day.com/jcr/cq/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:nt="http://www.jcp.org/jcr/nt/1.0"
          jcr:primaryType="cq:Page">
```

The `<jcr:root>` element represents the page node itself. Its only required attribute (beyond namespaces) is `jcr:primaryType="cq:Page"`.

### 5.2. `jcr:content` (Page Content)

```xml
<jcr:content jcr:primaryType="cq:PageContent"
             jcr:title="My Form Title"
             sling:resourceType="fd/af/components/guideContainer"
             cq:template="/conf/.../settings/wcm/templates/af-blank-v2">
```

| Attribute | Required | Description |
|-----------|----------|-------------|
| `jcr:primaryType` | Yes | Always `"cq:PageContent"` |
| `jcr:title` | Yes | Human-readable form title |
| `sling:resourceType` | Yes | Component type for rendering; typically `"fd/af/components/guideContainer"` |
| `cq:template` | Yes | Path to the AEM template definition |
| `cq:lastModified` | No | Last modification timestamp (e.g. `"{Date}2025-01-15T10:30:00.000+00:00"`) |
| `cq:lastModifiedBy` | No | Author of last modification |

### 5.3. `guideContainer`

The guide container is the top-level adaptive form container.

```xml
<guideContainer jcr:primaryType="nt:unstructured"
                sling:resourceType="fd/af/components/guideContainer"
                guideNodeClass="guideContainerNode"
                fd:version="2.1"
                dorType="generate"
                themeRef="/libs/fd/af/themes/..."
                dorTemplateRef="/conf/.../dor-template"
                redirect="/content/.../thankyou">
```

| Attribute | Required | Description |
|-----------|----------|-------------|
| `jcr:primaryType` | Yes | `"nt:unstructured"` |
| `sling:resourceType` | Yes | `"fd/af/components/guideContainer"` |
| `guideNodeClass` | Yes | `"guideContainerNode"` |
| `fd:version` | Yes | Form version; typically `"2.1"` |
| `dorType` | No | Document of Record type; `"generate"` or `"none"` |
| `themeRef` | No | Path to the theme client library |
| `dorTemplateRef` | No | Path to a custom DOR template |
| `redirect` | No | URL to redirect to after form submission |

### 5.4. `rootPanel`

The root panel wraps all form content.

```xml
<rootPanel jcr:primaryType="nt:unstructured"
           sling:resourceType="fd/af/components/panel"
           guideNodeClass="guideRootPanel"
           jcr:title="My Form Title"
           textIsRich="true">
```

| Attribute | Required | Description |
|-----------|----------|-------------|
| `jcr:primaryType` | Yes | `"nt:unstructured"` |
| `sling:resourceType` | Yes | Panel resource type (e.g. `"fd/af/components/panel"`) |
| `guideNodeClass` | Yes | `"guideRootPanel"` |
| `jcr:title` | Yes | Form title |
| `textIsRich` | No | `"true"` if the title may contain rich text |

The `rootPanel` contains:
- **`<items>`** — child components (panels, fields, etc.)
- **`<layout>`** — layout configuration
- **`<toolbar>`** — navigation/submit buttons (optional)

---

## 6. Component Reference

Every component in the form is an XML element nested inside an `<items>` container. Components are identified by two key attributes:

- **`guideNodeClass`** — the AEM Forms component class (determines behavior)
- **`sling:resourceType`** — the Sling resource type (determines rendering)

The element tag name is typically `<componentType_UUID>` (e.g. `textbox_a1b2c3d4...`), though some elements use fixed names (e.g. `rootPanel`, `guideContainer`, `toolbar`).

### 6.1. Common Attributes

These attributes appear on most or all components:

| Attribute | Type | Description |
|-----------|------|-------------|
| `jcr:primaryType` | String | Always `"nt:unstructured"` for form components |
| `sling:resourceType` | String | Sling resource type for rendering |
| `guideNodeClass` | String | AEM Forms component class |
| `name` | String | Logical field name (used in form data model) |
| `jcr:title` | String | Human-readable label |
| `visible` | TypedBoolean | `"{Boolean}true"` or `"{Boolean}false"` |
| `enabled` | TypedBoolean | `"{Boolean}true"` or `"{Boolean}false"` |
| `mandatory` | String | `"true"` or `"false"` (plain string, not typed Boolean) |
| `css` | String | CSS class(es) for styling |
| `textIsRich` | String | `"true"`, `"[true,true]"`, or `"[true,true,true]"` — indicates which text fields may contain HTML |
| `dorExclusion` | String/TypedBoolean | Exclude from Document of Record; `"true"`, `"{Boolean}true"`, or `"{Boolean}false"` |
| `dorFieldStyling` | String | DOR styling mode; typically `"Default"` |
| `dorExcludeTitle` | String | `"true"` to exclude title from DOR |
| `dorExcludeDescription` | String | `"true"` to exclude description from DOR |
| `assistPriority` | String | Accessibility assist priority; `"label"`, `"caption"`, `"custom"` |
| `jcr:created` | TypedDate | `"{Date}2025-01-01T00:00:00.000Z"` |
| `jcr:createdBy` | String | Author name |
| `jcr:lastModified` | TypedDate | `"{Date}2025-01-01T00:00:00.000Z"` |
| `jcr:lastModifiedBy` | String | Author name |

### 6.2. Panel (`guidePanel`)

A container for other components. Panels can be nested.

```xml
<panel_<uuid> jcr:primaryType="nt:unstructured"
              sling:resourceType="fd/af/components/panel"
              guideNodeClass="guidePanel"
              name="PN_MyPanel"
              jcr:title="Panel Title"
              textIsRich="true"
              dorExclusion="{Boolean}false"
              dorExcludeDescription="true"
              dorFieldStyling="Default"
              validateOnStepCompletion="{Boolean}false">
    <items jcr:primaryType="nt:unstructured"
           sling:resourceType="fd/af/layouts/gridFluidLayout2">
        <!-- child components here -->
    </items>
    <layout jcr:primaryType="nt:unstructured"
            sling:resourceType="fd/af/layouts/gridFluidLayout2"
            enableLayoutOptimization="{Boolean}true"
            nonNavigable="{Boolean}true"
            toolbarPosition="Bottom"/>
</panel_<uuid>>
```

**Panel-specific attributes:**

| Attribute | Description |
|-----------|-------------|
| `completionExpReq` | Completion expression requirement |
| `panelSetType` | Panel set type (e.g. for wizard steps) |
| `validateOnStepCompletion` | `"{Boolean}false"` — validate when stepping through wizard |
| `dorColspan` | Column span in DOR layout |
| `dorLayoutType` | DOR layout type |
| `dorNumCols` | Number of columns in DOR |

### 6.3. Text Box (`guideTextBox`)

Single-line text input.

```xml
<textbox_<uuid> jcr:primaryType="nt:unstructured"
                sling:resourceType="fd/af/components/controls/textbox"
                guideNodeClass="guideTextBox"
                name="TF_FieldName"
                jcr:title="Field Label"
                mandatory="true"
                visible="{Boolean}true"
                assistPriority="label"
                css="widget_textbox"
                textIsRich="[true,true,true]"
                maxChars="100">
    <cq:responsive jcr:primaryType="nt:unstructured">
        <default jcr:primaryType="nt:unstructured" offset="0" width="6"/>
    </cq:responsive>
</textbox_<uuid>>
```

**Text-box-specific attributes:**

| Attribute | Description |
|-----------|-------------|
| `maxChars` | Maximum character count |
| `multiLine` | `"true"` for multi-line text area |
| `placeholderText` | Placeholder hint text |

### 6.4. Numeric Box (`guideNumericBox`)

Numeric input field.

```xml
<numericbox_<uuid> jcr:primaryType="nt:unstructured"
                   sling:resourceType="fd/af/components/controls/numericbox"
                   guideNodeClass="guideNumericBox"
                   name="NF_Amount"
                   jcr:title="Amount"
                   mandatory="false"
                   visible="{Boolean}true"
                   textIsRich="[true,true,true]">
    <cq:responsive .../>
</numericbox_<uuid>>
```

**Numeric-specific attributes:**

| Attribute | Description |
|-----------|-------------|
| `validatePictureClause` | Validation pattern (e.g. `"num{z,zzz,zzz,zz9}"`) |
| `displayPictureClause` | Display format pattern |
| `displayPatternType` | `"custom"` or predefined pattern type |
| `displayIsSameAsValidate` | `"true"` if display format equals validation format |

### 6.5. Date Picker (`guideDatePicker`)

Date input with calendar picker.

```xml
<datepicker_<uuid> jcr:primaryType="nt:unstructured"
                   sling:resourceType="fd/af/components/controls/datepicker"
                   guideNodeClass="guideDatePicker"
                   name="DP_BirthDate"
                   jcr:title="Date of Birth"
                   mandatory="false"
                   visible="{Boolean}true"
                   defaultToCurrentDate="true"
                   placeholderText=""
                   textIsRich="[true,true]"
                   validatePictureClause="date{YYYY-MM-DD}"
                   validatePictureClauseMessage="Please enter the date ..."
                   validationPatternType="custom"
                   yearRangeFrom="100"
                   yearRangeTo="10">
    <cq:responsive .../>
</datepicker_<uuid>>
```

**Date-picker-specific attributes:**

| Attribute | Description |
|-----------|-------------|
| `defaultToCurrentDate` | `"true"` to default to today |
| `validatePictureClause` | Date validation pattern (e.g. `"date{YYYY-MM-DD}"`) |
| `validatePictureClauseMessage` | Error message for invalid date format |
| `validationPatternType` | `"custom"` or a predefined type |
| `displayPictureClause` | Date display format |
| `yearRangeFrom` | Years before current year for picker range |
| `yearRangeTo` | Years after current year for picker range |

### 6.6. Drop-Down List (`guideDropDownList`)

Select / dropdown input.

```xml
<dropdownlist_<uuid> jcr:primaryType="nt:unstructured"
                     sling:resourceType="fd/af/components/controls/dropdownlist"
                     guideNodeClass="guideDropDownList"
                     name="DD_Country"
                     jcr:title="Country"
                     mandatory="true"
                     visible="{Boolean}true"
                     options="[CH=Switzerland,DE=Germany,AT=Austria]"
                     textIsRich="[true,true]"
                     filteringAllowed="true"
                     sort="ascending">
    <cq:responsive .../>
</dropdownlist_<uuid>>
```

**Dropdown-specific attributes:**

| Attribute | Description |
|-----------|-------------|
| `options` | Option list in format `"[value1=label1,value2=label2,...]"` |
| `filteringAllowed` | `"true"` to enable type-ahead filtering |
| `sort` | `"ascending"`, `"descending"`, or absent for no sort |

### 6.7. Checkbox (`guideCheckBox`)

Checkbox input (single or group).

```xml
<checkbox_<uuid> jcr:primaryType="nt:unstructured"
                 sling:resourceType="fd/af/components/controls/checkbox"
                 guideNodeClass="guideCheckBox"
                 name="CB_Agree"
                 alignment="horizontal"
                 assistPriority="caption"
                 hideTitle="true"
                 visible="{Boolean}true"
                 options="[1=I agree to the terms]"
                 richTextOptions="true"
                 textIsRich="[true,true]">
    <cq:responsive .../>
</checkbox_<uuid>>
```

**Checkbox-specific attributes:**

| Attribute | Description |
|-----------|-------------|
| `options` | Option list in format `"[value1=label1,value2=label2,...]"` |
| `alignment` | `"horizontal"` or `"vertical"` |
| `richTextOptions` | `"true"` if option labels contain HTML |
| `hideTitle` | `"true"` to hide the field title |

### 6.8. Radio Button (`guideRadioButton`)

Radio button group.

```xml
<radiobutton_<uuid> jcr:primaryType="nt:unstructured"
                    sling:resourceType="fd/af/components/controls/radiobutton"
                    guideNodeClass="guideRadioButton"
                    name="RB_Choice"
                    jcr:title="Choose one"
                    mandatory="true"
                    alignment="vertical"
                    visible="{Boolean}true"
                    options="[a=Option A,b=Option B,c=Option C]"
                    richTextOptions="true"
                    textIsRich="[true,true,true,true]">
    <cq:responsive .../>
</radiobutton_<uuid>>
```

**Radio-specific attributes:** Same as Checkbox.

### 6.9. Text Draw (`guideTextDraw`)

Static text / heading display (not an input).

```xml
<textdraw_<uuid> jcr:primaryType="nt:unstructured"
                 sling:resourceType="fd/af/components/controls/textdraw"
                 guideNodeClass="guideTextDraw"
                 name="ST_Heading1"
                 _value="&lt;h2&gt;Section Title&lt;/h2&gt;"
                 css=""
                 textIsRich="true"
                 dorExclusion="true">
    <fd:rules jcr:primaryType="nt:unstructured"/>
    <cq:responsive .../>
</textdraw_<uuid>>
```

**TextDraw-specific attributes:**

| Attribute | Description |
|-----------|-------------|
| `_value` | The HTML content to display (XML-escaped) |
| `text` | Alternative to `_value` in some versions |
| `headingLevel` | Heading level as a plain tag number, `"1"` through `"6"` (e.g. `"2"` for an `<h2>`), when used as a heading -- verified against the real `AF_AABF.zip` fixture and the reference engine's own parser; not `"H1"`-`"H6"` as an earlier revision of this doc claimed |

### 6.10. Scribble (`guideScribble`)

Signature / drawing input.

```xml
<scribble_<uuid> jcr:primaryType="nt:unstructured"
                 sling:resourceType="fd/af/components/controls/scribble"
                 guideNodeClass="guideScribble"
                 name="SIG_Signature"
                 jcr:title="Signature">
    <cq:responsive .../>
</scribble_<uuid>>
```

### 6.11. Button (`guideButton`)

Toolbar or action button.

```xml
<submit jcr:primaryType="nt:unstructured"
        sling:resourceType="fd/af/components/submit"
        guideNodeClass="guideButton"
        jcr:title="Submit"
        dorExclusion="true"
        dorFieldStyling="Default"/>
```

Common button resource types:
- `fd/af/components/submit` — submit button
- `fd/af/components/previtemnav` — previous wizard step
- `fd/af/components/nextitemnav` — next wizard step
- `fd/af/components/controls/removebutton` — remove repeatable instance
- `fd/af/components/controls/tertiarybutton` — add repeatable instance

### 6.12. Fragment Reference

Reusable form fragments referenced via `fragRef`.

```xml
<fragment_<uuid> jcr:primaryType="nt:unstructured"
                 sling:resourceType="fd/af/components/panel"
                 guideNodeClass="guidePanel"
                 fragRef="/content/forms/af/fragments/my-fragment"
                 name="FRAG_Address">
</fragment_<uuid>>
```

| Attribute | Description |
|-----------|-------------|
| `fragRef` | JCR path to the referenced form fragment |

### 6.13. Summary: guideNodeClass Values

| `guideNodeClass` | Component | Description |
|-------------------|-----------|-------------|
| `guideContainerNode` | Guide Container | Top-level form container |
| `guideRootPanel` | Root Panel | Root panel of the form |
| `guidePanel` | Panel | Generic container panel |
| `guideTextBox` | Text Box | Single-line or multi-line text input |
| `guideNumericBox` | Numeric Box | Numeric input |
| `guideDatePicker` | Date Picker | Date input with calendar |
| `guideDropDownList` | Drop-Down List | Select/dropdown |
| `guideCheckBox` | Checkbox | Checkbox (single or group) |
| `guideRadioButton` | Radio Button | Radio button group |
| `guideTextDraw` | Text Draw | Static text / heading |
| `guideScribble` | Scribble | Signature / drawing pad |
| `guideButton` | Button | Action button (submit, nav, etc.) |
| `guideToolbar` | Toolbar | Toolbar container |
| `rootPanelNode` | Root Panel Node | Alternative root panel identifier |

---

## 7. Layout & Responsive Grid

### 7.1. `<items>` Container

Every panel (including `rootPanel`) wraps its children in an `<items>` element:

```xml
<items jcr:primaryType="nt:unstructured"
       sling:resourceType="fd/af/layouts/gridFluidLayout2">
    <!-- child components -->
</items>
```

The `sling:resourceType` on `<items>` specifies the layout renderer.

### 7.2. `<layout>` Element

Each panel also has a `<layout>` element that configures how children are arranged:

```xml
<layout jcr:primaryType="nt:unstructured"
        sling:resourceType="fd/af/layouts/gridFluidLayout2"
        enableLayoutOptimization="{Boolean}true"
        nonNavigable="{Boolean}true"
        toolbarPosition="Bottom"
        columns="2"/>
```

| Attribute | Description |
|-----------|-------------|
| `sling:resourceType` | Layout type; common values: `fd/af/layouts/gridFluidLayout2`, `fd/af/layouts/gridFluidLayout` |
| `enableLayoutOptimization` | `"{Boolean}true"` to optimize layout rendering |
| `nonNavigable` | `"{Boolean}true"` — panel children are all visible (not tabs/accordion) |
| `toolbarPosition` | `"Bottom"` or `"Top"` — where the toolbar appears |
| `columns` | Number of layout columns (for some layout types) |

### 7.3. `<cq:responsive>` Element

Individual components specify their column width within the responsive grid:

```xml
<cq:responsive jcr:primaryType="nt:unstructured">
    <default jcr:primaryType="nt:unstructured"
             offset="0"
             width="6"/>
</cq:responsive>
```

| Attribute | Description |
|-----------|-------------|
| `offset` | Number of columns to offset from the left (typically `"0"`) |
| `width` | Number of grid columns this component spans (out of 12) |

The `<default>` child represents the default breakpoint. Additional breakpoints (e.g. `<tablet>`, `<phone>`) can be added as siblings.

---

## 8. Rules & Scripts

### 8.1. `<fd:rules>` Element

Components can have business rules defined as child elements:

```xml
<fd:rules jcr:primaryType="nt:unstructured">
    <fd:init jcr:primaryType="nt:unstructured"
             description="Initialize field"
             jcr:title="init"
             fdType="init"
             script="this.value = 'default'"/>
    <fd:valueCommit jcr:primaryType="nt:unstructured"
                    description="On value change"
                    jcr:title="valueCommit"
                    fdType="valueCommit"
                    script="if(this.value === 'X') { other_field.visible = false; }"/>
</fd:rules>
```

| Child Element | Trigger |
|---------------|---------|
| `fd:init` | When the component initializes |
| `fd:valueCommit` | When the field value changes |
| `fd:click` | When the component is clicked |
| `fd:validate` | Custom validation |

| Attribute | Description |
|-----------|-------------|
| `fdType` | Rule type identifier |
| `script` | JavaScript expression or statement |
| `description` | Human-readable description |
| `jcr:title` | Rule name |

### 8.2. `<fd:scripts>` Element

Similar to `fd:rules`, but for more complex scripting:

```xml
<fd:scripts jcr:primaryType="nt:unstructured">
    <fd:init jcr:primaryType="nt:unstructured"
             script="// JavaScript code here"/>
</fd:scripts>
```

---

## 9. DAM Asset Content XML

The DAM asset `.content.xml` provides metadata for the Forms Manager UI.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:fd="http://www.adobe.com/aemfd/fd/1.0"
          xmlns:dam="http://www.day.com/dam/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:nt="http://www.jcp.org/jcr/nt/1.0"
          jcr:primaryType="dam:Asset">
    <jcr:content jcr:primaryType="dam:AssetContent"
                 sling:resourceType="fd/fm/af/render"
                 guide="1"
                 type="guide">
        <metadata fd:version="1.1"
                  jcr:primaryType="nt:unstructured"
                  allowedRenderFormat="HTML"
                  author="blueprint"
                  dorType="generate"
                  dorTemplateRef="/conf/.../dor-template"
                  formmodel="none"
                  hasCustomThumbnail="{Boolean}false"
                  themeRef="/libs/fd/af/themes/..."
                  title="My Form Title"/>
    </jcr:content>
</jcr:root>
```

### DAM Attribute Reference

**`jcr:content`:**

| Attribute | Value | Description |
|-----------|-------|-------------|
| `jcr:primaryType` | `"dam:AssetContent"` | Fixed |
| `sling:resourceType` | `"fd/fm/af/render"` | Render type for adaptive forms |
| `guide` | `"1"` | Identifies as a guide/form |
| `type` | `"guide"` | Asset type |

**`metadata`:**

| Attribute | Description |
|-----------|-------------|
| `fd:version` | Metadata version (e.g. `"1.1"`) |
| `jcr:primaryType` | `"nt:unstructured"` |
| `allowedRenderFormat` | `"HTML"` |
| `author` | Form author |
| `dorType` | `"generate"` or `"none"` |
| `dorTemplateRef` | Path to DOR template |
| `formmodel` | Data model type; `"none"`, `"xsd"`, `"xdp"` |
| `hasCustomThumbnail` | `"{Boolean}false"` |
| `themeRef` | Path to theme |
| `title` | Form title |

---

## 10. Translation Dictionaries

AEM Forms uses Sling i18n dictionaries for form translations. Each locale has a separate XML file stored under:

```
<form-path>/_jcr_content/guideContainer/assets/dictionary/<locale>.xml
```

### Dictionary Format

```xml
<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:mix="http://www.jcp.org/jcr/mix/1.0"
          xmlns:nt="http://www.jcp.org/jcr/nt/1.0"
          jcr:language="de"
          jcr:mixinTypes="[mix:language]"
          jcr:primaryType="sling:Folder"
          sling:basename="/content/forms/af/<path>/<form>/jcr:content/guideContainer/assets/dictionary">
    <fd_<uuid> jcr:mixinTypes="[sling:Message]"
               jcr:primaryType="nt:folder"
               sling:key="fd_English Source Text"
               sling:message="Translated text in target language"/>
    <!-- more entries -->
</jcr:root>
```

### Dictionary Root Attributes

| Attribute | Description |
|-----------|-------------|
| `jcr:language` | ISO 639-1 language code (e.g. `"de"`, `"fr"`, `"it"`) |
| `jcr:mixinTypes` | `"[mix:language]"` |
| `jcr:primaryType` | `"sling:Folder"` |
| `sling:basename` | JCR path to the dictionary folder (used for resolution) |

### Dictionary Entry Attributes

| Attribute | Description |
|-----------|-------------|
| `jcr:mixinTypes` | `"[sling:Message]"` |
| `jcr:primaryType` | `"nt:folder"` |
| `sling:key` | Translation key; prefixed with `"fd_"` followed by the master-language text |
| `sling:message` | Translated text in the target language |

**Element naming:** Each entry element is named `fd_<UUID>` where the UUID is derived deterministically from the `sling:key`.

---

## 11. Intermediate Folder Content XML

Each directory level in the JCR path needs a `.content.xml` file that defines the folder node type. The standard folder types are:

### Repository root (`jcr_root/.content.xml`)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:rep="internal"
          jcr:mixinTypes="[rep:AccessControllable,rep:RepoAccessControllable]"
          jcr:primaryType="rep:root"
          sling:resourceType="sling:redirect"
          sling:target="/index.html"/>
```

### Content folder (`jcr_root/content/.content.xml`)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:cq="http://www.day.com/jcr/cq/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:rep="internal"
          jcr:mixinTypes="[rep:AccessControllable]"
          jcr:primaryType="sling:OrderedFolder">
    <rep:policy/>
    <dam/>
    <forms/>
</jcr:root>
```

Note: Child element stubs (`<dam/>`, `<forms/>`) indicate child nodes that exist but whose content is serialized in their own `.content.xml` files.

### Forms / AF folders

```xml
<!-- content/forms/.content.xml -->
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          jcr:primaryType="sling:OrderedFolder">
    <af/>
</jcr:root>

<!-- content/forms/af/.content.xml -->
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:rep="internal"
          jcr:mixinTypes="[rep:AccessControllable]"
          jcr:primaryType="sling:Folder"
          hidden="true"/>
```

### DAM / formsanddocuments folders

```xml
<!-- content/dam/.content.xml -->
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:rep="internal"
          jcr:mixinTypes="[rep:AccessControllable]"
          jcr:primaryType="sling:Folder"/>

<!-- content/dam/formsanddocuments/.content.xml -->
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:rep="internal"
          jcr:mixinTypes="[rep:AccessControllable]"
          jcr:primaryType="sling:Folder"
          hidden="true"/>
```

### Intermediate path segments

All intermediate path segments (e.g. `ajila-forms-ubs/`, `output/`, `Germany_Tranch_1/`) use:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          jcr:primaryType="sling:OrderedFolder"/>
```

---

## 12. META-INF / Vault Metadata

### 12.1. `MANIFEST.MF`

Standard Java manifest with content-package metadata.

```
Manifest-Version: 1.0
Content-Package-Id: fd/export:PackageName
Content-Package-Roots: /content/forms/af/...,/content/dam/formsanddocuments/...
Content-Package-Type: mixed
```

Lines are wrapped at 72 bytes using continuation lines (leading space).

### 12.2. `filter.xml`

Defines which JCR subtrees the package manages.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<workspaceFilter version="1.0">
    <filter root="/content/forms/af/<path>/<form-code>"/>
    <filter root="/content/dam/formsanddocuments/<path>/<form-code>"/>
</workspaceFilter>
```

Each `<filter>` element can optionally contain `<include>` and `<exclude>` child elements with `pattern` attributes for fine-grained control.

### 12.3. `properties.xml`

Java properties file with package metadata.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE properties SYSTEM "http://java.sun.com/dtd/properties.dtd">
<properties>
    <comment>FileVault Package Properties</comment>
    <entry key="packageType">mixed</entry>
    <entry key="group">fd/export</entry>
    <entry key="name">PackageName</entry>
    <entry key="version"></entry>
    <entry key="created">2025-01-01T00:00:00.000+00:00</entry>
    <entry key="createdBy">blueprint</entry>
    <entry key="lastModified">2025-01-01T00:00:00.000+00:00</entry>
    <entry key="lastModifiedBy">blueprint</entry>
    <entry key="lastWrapped">2025-01-01T00:00:00.000+00:00</entry>
    <entry key="lastWrappedBy">blueprint</entry>
    <entry key="buildCount">1</entry>
    <entry key="packageFormatVersion">2</entry>
    <entry key="dependencies"></entry>
</properties>
```

### 12.4. `config.xml`

FileVault aggregation configuration. Defines how JCR node types are mapped to filesystem serialization strategies.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<vaultfs version="1.1">
    <aggregates>
        <aggregate type="file" title="File Aggregate"/>
        <aggregate type="filefolder" title="File/Folder Aggregate"/>
        <aggregate type="nodetype" title="Node Type Aggregate"/>
        <aggregate type="full" title="Full Coverage Aggregate">
            <matches>
                <include nodeType="rep:AccessControl" respectSupertype="true"/>
                <include nodeType="rep:Policy" respectSupertype="true"/>
                <include nodeType="cq:Widget" respectSupertype="true"/>
                <include nodeType="cq:EditConfig" respectSupertype="true"/>
                <include nodeType="cq:WorkflowModel" respectSupertype="true"/>
                <include nodeType="vlt:FullCoverage" respectSupertype="true"/>
                <include nodeType="mix:language" respectSupertype="true"/>
                <include nodeType="sling:OsgiConfig" respectSupertype="true"/>
            </matches>
        </aggregate>
        <!-- ... additional aggregates ... -->
    </aggregates>
    <handlers>
        <handler type="folder"/>
        <handler type="file"/>
        <handler type="nodetype"/>
        <handler type="generic"/>
    </handlers>
</vaultfs>
```

### 12.5. `nodetypes.cnd`

Compact Node Type Definition file declaring all JCR node types used in the package. See [Section 4](#4-jcr-node-type-definitions) for the types defined.

Format uses CND notation:

```
<'prefix'='namespace-uri'>

[nodeTypeName] > supertypes
  orderable? primaryitem?
  - propertyName (type) attributes
  + childNodeName (requiredTypes) = defaultType attributes
```

### 12.6. `definition/.content.xml`

Package definition as a JCR node.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:vlt="http://www.day.com/jcr/vault/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:nt="http://www.jcp.org/jcr/nt/1.0"
          jcr:primaryType="vlt:PackageDefinition"
          buildCount="1"
          group="fd/export"
          name="PackageName"
          version=""
          jcr:created="{Date}2025-01-01T00:00:00.000+00:00"
          jcr:createdBy="blueprint"
          jcr:lastModified="{Date}2025-01-01T00:00:00.000+00:00"
          jcr:lastModifiedBy="blueprint"
          lastWrapped="{Date}2025-01-01T00:00:00.000+00:00"
          lastWrappedBy="blueprint">
    <filter jcr:primaryType="nt:unstructured">
        <f0 jcr:primaryType="nt:unstructured"
            mode="replace"
            root="/content/forms/af/<path>/<form-code>"
            rules="[]"/>
        <f1 jcr:primaryType="nt:unstructured"
            mode="replace"
            root="/content/dam/formsanddocuments/<path>/<form-code>"
            rules="[]"/>
    </filter>
</jcr:root>
```

| Attribute | Description |
|-----------|-------------|
| `mode` | Import mode: `"replace"`, `"merge"`, `"update"` |
| `root` | JCR path managed by this filter |
| `rules` | Array of include/exclude rules; `"[]"` for none |

---

## 13. Attribute Value Type Hints

JCR property values in `.content.xml` use type-hint prefixes to indicate the property type. Without a prefix, the value is treated as a `String`.

| Prefix | JCR Type | Example |
|--------|----------|---------|
| *(none)* | `String` | `"hello"` |
| `{Boolean}` | `Boolean` | `"{Boolean}true"` |
| `{Long}` | `Long` | `"{Long}42"` |
| `{Double}` | `Double` | `"{Double}3.14"` |
| `{Decimal}` | `Decimal` | `"{Decimal}99.99"` |
| `{Date}` | `Date` | `"{Date}2025-01-01T00:00:00.000Z"` |
| `{Name}` | `Name` | `"{Name}nt:unstructured"` |
| `{Path}` | `Path` | `"{Path}/content/forms/af"` |
| `{Binary}` | `Binary` | (typically not inline) |

### Multi-value Properties

Arrays are encoded with `[...]` syntax:

```xml
jcr:mixinTypes="[rep:AccessControllable,rep:RepoAccessControllable]"
options="[a=Option A,b=Option B]"
textIsRich="[true,true,true]"
```

---

## 14. Component Families

AEM Adaptive Forms has two component families. This spec primarily documents **Foundation Components**, which are used in the observed packages.

### Foundation Components

Resource type pattern: `fd/af/components/**`

| Resource Type | Description |
|---------------|-------------|
| `fd/af/components/guideContainer` | Form container |
| `fd/af/components/panel` | Panel |
| `fd/af/components/rootPanel` | Root panel |
| `fd/af/components/controls/textbox` | Text box |
| `fd/af/components/controls/numericbox` | Numeric box |
| `fd/af/components/controls/datepicker` | Date picker |
| `fd/af/components/controls/dropdownlist` | Drop-down list |
| `fd/af/components/controls/checkbox` | Checkbox |
| `fd/af/components/controls/radiobutton` | Radio button |
| `fd/af/components/controls/textdraw` | Static text / heading |
| `fd/af/components/controls/scribble` | Signature/scribble |
| `fd/af/components/controls/removebutton` | Remove repeatable instance |
| `fd/af/components/controls/tertiarybutton` | Tertiary button (e.g. add repeatable) |
| `fd/af/components/submit` | Submit button |
| `fd/af/components/previtemnav` | Previous step navigation |
| `fd/af/components/nextitemnav` | Next step navigation |
| `fd/af/layouts/gridFluidLayout2` | Responsive grid layout (v2) |
| `fd/af/layouts/gridFluidLayout` | Responsive grid layout (v1) |
| `fd/af/layouts/toolbarCommonLayout` | Toolbar layout |

### Custom / Overlay Components

Projects can overlay foundation components with custom implementations:

```
<custom-project>/components/controls/textbox
<custom-project>/components/controls/checkbox
...
```

These extend the foundation behavior while allowing project-specific customizations.

### Core Components (AEM Forms as a Cloud Service)

Resource type pattern: `core/fd/components/form/**`

Core Components are the newer recommended approach for AEM Forms as a Cloud Service. They use a different structure and are not covered in detail in this spec. Key differences:
- Resource types start with `core/fd/components/form/`
- They do not use `guideNodeClass`
- They use a different set of properties and child node structures
- They are based on the AEM WCM Core Components architecture

---

## Appendix: Repeatable Panels

Repeatable panels allow users to add/remove instances of a section dynamically. They use a nested panel structure:

```xml
<repeatable_<uuid> jcr:primaryType="nt:unstructured"
                   sling:resourceType="fd/af/components/panel"
                   guideNodeClass="guidePanel"
                   name="RPT_Section"
                   jcr:title="Repeatable Section">
    <items jcr:primaryType="nt:unstructured"
           sling:resourceType="fd/af/layouts/gridFluidLayout2">
        <!-- Inner repeatable panel with min/max -->
        <repeatableInner jcr:primaryType="nt:unstructured"
                         sling:resourceType="fd/af/components/panel"
                         guideNodeClass="guidePanel"
                         name="PN_RPT_Section"
                         minOccur="1"
                         maxOccur="20">
            <items ...>
                <!-- actual fields go here -->
            </items>
            <layout .../>
            <toolbar jcr:primaryType="nt:unstructured">
                <removebutton jcr:primaryType="nt:unstructured"
                              sling:resourceType="fd/af/components/controls/removebutton"
                              guideNodeClass="guideButton"
                              dorExclusion="true"
                              jcr:title="Remove"/>
            </toolbar>
        </repeatableInner>
        <!-- Add button outside the repeatable inner -->
        <addbutton jcr:primaryType="nt:unstructured"
                   sling:resourceType="fd/af/components/controls/tertiarybutton"
                   guideNodeClass="guideButton"
                   dorExclusion="true"
                   jcr:title="Add RPT_Section"
                   type="button"/>
    </items>
    <layout .../>
</repeatable_<uuid>>
```

| Attribute | Description |
|-----------|-------------|
| `minOccur` | Minimum number of repeatable instances |
| `maxOccur` | Maximum number of repeatable instances |

The outer panel contains:
1. The inner repeatable panel template (with `minOccur`/`maxOccur`)
2. An "Add" button (tertiary button) to create new instances

The inner repeatable panel contains:
1. The actual form fields
2. A toolbar with a "Remove" button to delete the instance

---

## Appendix: XML Formatting Convention

AEM's export format (and the Blueprint project) uses a one-attribute-per-line formatting style for elements with multiple attributes:

```xml
<textbox_abc123
    jcr:primaryType="nt:unstructured"
    sling:resourceType="fd/af/components/controls/textbox"
    guideNodeClass="guideTextBox"
    name="TF_Name"
    jcr:title="Full Name"
    mandatory="true"
    visible="{Boolean}true">
    <cq:responsive jcr:primaryType="nt:unstructured">
        <default jcr:primaryType="nt:unstructured"
                 offset="0"
                 width="6"/>
    </cq:responsive>
</textbox_abc123>
```

Elements with a single attribute remain on one line. Self-closing elements use `/>`.

---

## 15. Expressions & Scripting Model

Adaptive Forms use JavaScript as their expression language. All expressions are valid JavaScript and use the adaptive forms scripting model APIs (GuideBridge). The scripting model is ECMAScript 5 (ES5) — newer ECMAScript versions (ES6+) are **not supported** in Foundation Components.

### 15.1. Expression Types

Each form component supports specific expression types, configured either via `fd:rules`/`fd:scripts` child nodes in the content XML or via the rule editor UI.

| Expression Type | Applies To | Return Type | Trigger | Description |
|-----------------|------------|-------------|---------|-------------|
| **Access (Enablement)** | Fields | Boolean | Value change | Enables/disables a field. `true` = enabled. |
| **Calculate** | Fields | Field-compatible value | Dependent value change | Auto-computes a field value. E.g. `field2.value + field3.value` |
| **Click** | Buttons | void | Click event | Action on button click. E.g. `guideBridge.submit()` |
| **Initialization Script** | Fields, Panels | void | Form init / prefill complete | Runs when field first renders. E.g. `if(this.value==null) this.value='default';` |
| **Options** | Dropdowns | String[] | Dependent value change | Dynamically fills dropdown options. Returns array of `"value=label"` strings. |
| **Summary** | Accordion child panels | String | Init / dependent change | Computes dynamic title for accordion child panels. |
| **Validate** | Fields | Boolean | Value change | Custom validation. `false` = invalid. |
| **Value Commit** | Fields | void | Value commit (blur) | Fires when user changes and commits a value. E.g. `this.value=this.value.toUpperCase()` |
| **Visibility** | Fields, Panels | Boolean | Dependent value change | Controls visibility. `false` = hidden. |
| **Step Completion** | Wizard panels | Boolean | Step navigation | Prevents wizard navigation if `false`. Typically uses `guideBridge.validate()`. |

### 15.2. Scripting API Reference

The scripting model provides access to form objects via their `name` property:

```javascript
// Access field value
field1.value

// Show/hide
field1.visible = false;
panel1.visible = true;

// Enable/disable
field1.enabled = false;

// Mandatory
field1.mandatory = true;

// Validation
field1.validationsDisabled = true;  // skip validation for hidden fields

// Repeatable panel instance management
panel1.instanceManager.addInstance();
panel1.instanceIndex;  // current instance index
_panel1.removeInstance(panel1.instanceIndex);
```

### 15.3. GuideBridge API

GuideBridge is the primary API for interacting with adaptive forms from JavaScript:

```javascript
// Submit form
guideBridge.submit();

// Validate form
guideBridge.validate(errorList, somExpression);

// Reset form
guideBridge.reset();

// Set focus
guideBridge.setFocus(somExpression, 'nextItem');

// Listen for events (external scripts)
window.addEventListener("bridgeInitializeStart", function(evnt) {
    var gb = evnt.detail.guideBridge;
    gb.connect(function() {
        // Form initialized
    });
});

// Element value change listener
guideBridge.on("elementValueChanged", function(event, data) {
    // React to value changes
});
```

### 15.4. Set Property Rule Actions

The rule editor's "Set Property" action can modify these component properties at runtime:

| Property | Type | Description |
|----------|------|-------------|
| `visible` | Boolean | Show/hide |
| `enabled` | Boolean | Enable/disable |
| `mandatory` | Boolean | Required/optional |
| `value` | Number, String, Date | Field value |
| `title` | String | Component title |
| `dorExclusion` | Boolean | Exclude from DOR |
| `chartType` | String | Chart type |
| `validationsDisabled` | Boolean | Disable validations |
| `validateExpMessage` | String | Validation error message |
| `items` | List | Dynamic option items |
| `valid` | Boolean | Validity state |
| `errorMessage` | String | Error message |

### 15.5. Rule Actions

Actions available in rule editor expressions:

| Action | Description |
|--------|-------------|
| **Show** / **Hide** | Toggle component visibility |
| **Enable** / **Disable** | Toggle component enabled state |
| **Set Value Of** | Compute and set field value (literal, expression, function, or service output) |
| **Set Property** | Modify a component property dynamically |
| **Clear Value Of** | Clear a field's value |
| **Set Focus** | Move focus to a component |
| **Save Form** | Save form state |
| **Submit Form(s)** | Trigger form submission |
| **Reset Form** | Reset all fields to defaults |
| **Validate Form** | Run form validation |
| **Add Instance** | Add a repeatable panel instance |
| **Remove Instance** | Remove a repeatable panel instance |
| **Invoke Service** | Call a Form Data Model service |
| **Navigate To** | Navigate to another form, URL, or asset |
| **Set Options Of** | Dynamically populate a dropdown/checkbox |

### 15.6. Rule Editor Operators

| Operator | Description |
|----------|-------------|
| Is Equal To | Equality comparison |
| Is Not Equal To | Inequality comparison |
| Starts With | String prefix match |
| Ends With | String suffix match |
| Contains | Substring match |
| Is Empty | Null/empty check |
| Is Not Empty | Non-null/non-empty check |
| Has Selected | Selected option check (checkbox, dropdown, radio) |
| Is Initialized (event) | Component render event |
| Is Changed (event) | Value change event |

---

## 16. Submit Actions

Adaptive Forms support several built-in submit actions. The submit action is configured in the `guideContainer` or form container properties.

### 16.1. Built-in Submit Actions

| Submit Action | Description |
|---------------|-------------|
| **Submit to REST Endpoint** | POST/GET form data to a REST URL. Supports both internal AEM paths and external URLs. |
| **Send Email** | Sends email with form data in predefined format. |
| **Send PDF via Email** | Sends PDF with form data (XFA-based forms only). |
| **Invoke a Forms Workflow** | Submits to Adobe LiveCycle or AEM Forms on JEE process. |
| **Submit using Form Data Model** | Writes data back to the configured FDM data source. |
| **Forms Portal Submit Action** | Makes data available via AEM Forms Portal. |
| **Invoke an AEM Workflow** | Triggers an AEM workflow with submitted data, attachments, and DOR. |
| **Submit to Power Automate** | Sends data to Microsoft Power Automate Cloud Flow. |
| **Submit to SharePoint List** | Connects form to Microsoft SharePoint storage. |

### 16.2. AEM Workflow Submit Payload

When using "Invoke an AEM Workflow", the submit action places the following at the workflow payload location:

| Item | Configuration | Example |
|------|---------------|---------|
| **Data file** | `Data File Path` | `/addresschange/data.xml` |
| **Attachments** | `Attachment Path` | Folder name relative to payload |
| **Document of Record** | `Document of Record Path` | `/addresschange/DoR.pdf` |

### 16.3. Server-Side Revalidation

AEM Forms can revalidate on the server after submission. Enabled via the `Revalidate on server` option in the Adaptive Form Container properties. Server-side revalidation covers:

- Required field validation
- Validation Picture Clause patterns
- Validation Expressions

---

## 17. Prefill Data Structure

Adaptive forms can be prefilled with data using XML or JSON. The prefill data structure has two sections:

### 17.1. XML Prefill Structure

```xml
<?xml version="1.0" encoding="UTF-8"?>
<afData>
    <afBoundData>
        <!-- Data matching the form model schema (XSD, XFA, FDM) -->
        <employeeData>
            <name>John Doe</name>
            <department>Engineering</department>
        </employeeData>
    </afBoundData>
    <afUnboundData>
        <!-- Data for fields without bindRef -->
        <data>
            <textbox>Hello World</textbox>
            <numericbox>12</numericbox>
        </data>
    </afUnboundData>
</afData>
```

### 17.2. JSON Prefill Structure

```json
{
    "afData": {
        "afBoundData": {
            "employeeData": {
                "name": "John Doe",
                "department": "Engineering"
            }
        },
        "afUnboundData": {
            "data": {
                "textbox": "Hello World",
                "numericbox": "12"
            }
        }
    }
}
```

### 17.3. Submitted Data Structure

Submitted data follows the same structure as prefill data:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<afData>
    <afUnboundData>
        <data>
            <radiobutton>2</radiobutton>
            <checkbox>2</checkbox>
            <textbox>User Input</textbox>
        </data>
    </afUnboundData>
    <afBoundData>
        <!-- Bound data matching form model -->
    </afBoundData>
    <afSubmissionInfo>
        <stateOverrides/>
        <signers/>
        <afPath>/content/dam/formsanddocuments/my-form</afPath>
        <afSubmissionTime>20250101120000</afSubmissionTime>
    </afSubmissionInfo>
</afData>
```

### 17.4. Prefill Data Rules

| Scenario | Bound Data Location | Unbound Data Location |
|----------|---------------------|-----------------------|
| With `afData` wrapper | `afData/afBoundData` | `afData/afUnboundData/data` |
| Without wrapper | Starts from schema root element | Not applicable |
| No form model | Not applicable | `afData/afUnboundData/data` |

**Prefill protocols supported:**
- `crx://` — Load from JCR repository (node must have `jcr:data` property)
- `file://` — Load from server filesystem
- `https://` — Load from external URL
- `service://` — Load from OSGI prefill service

---

## 18. Picture Clause Patterns

Picture clauses control how field values are formatted for display and validation. They are used in `validatePictureClause`, `displayPictureClause`, and related attributes.

### 18.1. Date Patterns

Syntax: `date{<pattern>}` or predefined `date.short{}`, `date.medium{}`, `date.long{}`, `date.full{}`

Default pattern: `{MMM D, YYYY}`

| Symbol | Description | Example |
|--------|-------------|---------|
| `D` | 1-2 digit day (1-31) | `5` |
| `DD` | Zero-padded day (01-31) | `05` |
| `M` | 1-2 digit month (1-12) | `3` |
| `MM` | Zero-padded month (01-12) | `03` |
| `MMM` | Abbreviated month name (locale-dependent) | `Mar` |
| `MMMM` | Full month name (locale-dependent) | `March` |
| `EEE` | Abbreviated weekday name | `Mon` |
| `EEEE` | Full weekday name | `Monday` |
| `YY` | 2-digit year (00=2000, 29=2029, 30=1930, 99=1999) | `25` |
| `YYYY` | 4-digit year | `2025` |

**Examples:**
- `date{YYYY-MM-DD}` → `2025-01-15`
- `date{DD.MM.YYYY}` → `15.01.2025`
- `date{MMMM D, YYYY}` → `January 15, 2025`

### 18.2. Numeric Patterns

Syntax: `num{<pattern>}` or predefined `num.integer{}`, `num.decimal{}`, `num.currency{}`, `num.percent{}`

| Symbol | Description |
|--------|-------------|
| `9` | Single digit; shows zero if input is empty |
| `Z` | Single digit; shows space if input is empty or zero |
| `z` | Single digit; shows nothing if input is empty or zero |
| `E` | Exponent part of floating-point number |
| `.` | Decimal radix (locale-dependent) |
| `,` | Grouping separator (locale-dependent) |
| `$` | Currency symbol (locale-dependent) |
| `%` | Percent symbol (locale-dependent) |
| `S` / `s` | Minus sign if negative; space/plus if positive |
| `CR` / `cr` | Credit symbol if negative; nothing otherwise |
| `(` / `)` | Parentheses if negative; space otherwise |

**Examples:**
- `num{z,zzz,zzz,zz9}` → `10,000`
- `num{z,zzz,zz9.99}` → `1,234.56`
- `num{$z,zzz,zz9.99}` → `$1,234.56`

### 18.3. Text Patterns

Syntax: `text{<pattern>}`

| Symbol | Description |
|--------|-------------|
| `A` | Single alphabetic character |
| `X` | Single character (any) |
| `O` | Single alphanumeric character |
| `0` (zero) | Single alphanumeric character |
| `9` | Single digit |

**Examples:**
- `text{999-999-9999}` → phone number format
- `text{AAAA-9999}` → 4 letters + hyphen + 4 digits

---

## 19. Form Data Model Binding

Adaptive forms can bind to data models for prefilling and submitting data. The binding is configured via the `bindRef` attribute on form components and the form model type on the page.

### 19.1. Supported Form Models

| Form Model | `formmodel` Value | Description |
|------------|-------------------|-------------|
| **None** | `"none"` | No data model binding; fields are unbound |
| **XML Schema** | `"xsd"` | Fields bind to elements/attributes in an XSD |
| **JSON Schema** | `"jsonschema"` | Fields bind to properties in a JSON Schema |
| **XFA Template** | `"xdp"` | Fields bind to XFA form template elements |
| **Form Data Model** | `"fdm"` | Fields bind to FDM entity properties |

### 19.2. `bindRef` Attribute

The `bindRef` attribute on a component specifies the XPath or JSON path to the data model element:

```xml
<textbox_<uuid> jcr:primaryType="nt:unstructured"
                sling:resourceType="fd/af/components/controls/textbox"
                guideNodeClass="guideTextBox"
                name="TF_Name"
                bindRef="/employee/name"
                jcr:title="Employee Name">
</textbox_<uuid>>
```

- **Bound fields** have a non-empty `bindRef` value
- **Unbound fields** have no `bindRef` (or empty); their data goes to `afUnboundData/data`
- Fields are identified by their `name` attribute in submitted unbound data

### 19.3. `autofillFieldKeyword` Attribute

The `autofillFieldKeyword` attribute enables browser autofill for a field by specifying the HTML autocomplete hint:

```xml
autofillFieldKeyword="name"
autofillFieldKeyword="email"
autofillFieldKeyword="tel"
```

---

## Appendix: Observed Deviations (u2s)

This appendix is maintained by `u2s-mapper-aem` alongside its own
`catalog.rs` (the stock Foundation-component table this appendix informs),
and records points where the fixture package
(`crates/u2s-mapper-aem/tests/fixtures/AF_AABF.zip`, a real,
human-authored UBS form) diverges from — or settles a question left open
by — the spec above. Copied into this repo (from
`ajila-forms-conversion-engine/specs/AEM.md`, upstream commit
`aee5aef09e47818fe99558aeb96d110be842b570`) specifically so this appendix
has somewhere to live next to the code it documents; add to it as the
decoder encounters more.

- **§14 overlay components confirmed in practice.** The fixture's
  `ajila-forms-customers/ajila-forms-ubs/components/controls/*` resource
  types are overlays of the foundation `fd/af/components/controls/*`
  components this spec documents — same semantics, project-specific JCR
  path. `u2s-mapper-aem`'s decoder treats the prefix as a suffix match
  against the same stock table (`catalog::FOUNDATION`), not a distinct
  component family.
- **§16 submit actions: `fd/af/components/nextitemnav` vs
  `fd/af/components/actions/nextitemnav`.** The fixture's toolbar action
  nodes (`nextitemnav`, `submit`, `previtemnav`) sit under
  `fd/af/components/actions/*`, not the `fd/af/components/*` this spec's
  §6 component table lists. `guidebutton` stays under
  `fd/af/components/*`. Follow the fixture.
- **§7 toolbar layout: `fd/af/layouts/toolbar/defaultToolbarLayout`**, not
  `fd/af/layouts/toolbarCommonLayout`. Follow the fixture.
- **`rootPanel`'s own resource type is `fd/af/components/rootPanel`**,
  settling the choice between this spec's §5.4 (`fd/af/components/panel`)
  and §14's own table entry in favour of **§14**.
- **`fd:version="1.1"`** in the fixture, not `"2.1"`, on `guideContainer`.
- **`guideContainer` has its own `layout` child**,
  `sling:resourceType="fd/af/layouts/defaultGuideLayout"` — a sibling of
  `rootPanel`, not folded into it. This crate's own writer does not emit
  this element at all today; that is a real gap, not a stylistic choice,
  since nothing else in the tree carries this information.
- **`rootPanel` itself has *two* children the current writer conflates
  into one.** The fixture's `rootPanel` carries a `layout` child whose
  `sling:resourceType` is the UBS wizard layout
  (`ajila-forms-customers/ajila-forms-ubs/layouts/panel/wizard`), separate
  from its `items` child, which itself carries
  `sling:resourceType="fd/af/layouts/gridFluidLayout2"` (the pages live
  inside `items`, not inside `layout`). This crate's writer currently
  emits one `layout` element carrying `gridFluidLayout2` plus
  `toolbarPosition`, and an `items` element with no `sling:resourceType`
  at all — the two elements' own resource types are effectively swapped,
  and the wizard-specific layout type is missing entirely. Not yet fixed;
  flagged here with the exact fixture shape so the correction can be made
  and verified against a real round-trip rather than guessed.
- **`toolbar` also wraps its buttons in `items`, plus a separate `layout`
  child** — both carrying `sling:resourceType="fd/af/layouts/toolbar/
  defaultToolbarLayout"`. The current writer puts toolbar buttons directly
  under `toolbar` with no `items` wrapper and no separate `layout` sibling.
  Not yet fixed, for the same reason as `rootPanel`'s own gap above.
- **Visibility rules use `fd:rules`/`fd:visible` JSON**, not a synthesized
  `fd:initialize` script. This spec's §8 does not fully settle the shape;
  follow the fixture's JSON convention.
- **A wizard step's title is not `jcr:title`.** The fixture's steps use a
  sibling `panel_title_*` panel containing a heading-level static text
  (`titledraw`) — and that sibling panel is not always title-only; one
  observed instance also carries a radio button. Treat `panel_title_*` as
  an ordinary authored panel, not a title synthesis target.
- **DAM `<dictionary>` node list can disagree with the dictionary files
  actually shipped.** The fixture's DAM asset lists 4 languages
  (`de, de-ch, en, es`) while 7 dictionary XMLs exist under
  `assets/dictionary`. Treat this as a warning-level inconsistency to
  carry faithfully, not an error.
- **Dictionary entry element names are not fully reconstructable.** Most
  (255 of 311 in the fixture) are `fd_` + UUIDv5(`NAMESPACE_URL`,
  `sling:key`), but a meaningful minority are not derivable from any
  standard scheme — evidence the corpus was assembled by more than one
  tool over time. A round-trip comparison must key dictionary entries by
  `(sling:key, sling:message)`, never by element name.
- **`options="[value=label,...]"` disambiguation.** Confirmed against the
  fixture: split on the *first* `=` (a label may contain `=`, a value may
  not), and `,` inside a label is escaped as `\,`.
