# Manual changes and open feedback, Italy (033)

Two sources, kept together because they describe the same review round:

1. The Confluence page **Manual changes - Italy (033)**
   (`https://ajila.atlassian.net/wiki/spaces/UBS/pages/5528092678/Manual+changes+-+Italy+033`,
   Marco Arnaiz), which lists what still has to be applied by hand after the
   automated conversion and the sweeps have run.
2. The review notes from the QA session of 2026-08-26, as handed over by the
   owner. Each note carries the person it was assigned to, where one was named.

Anything on this page that the engine can do deterministically has either been
implemented (see the entries in `consistent-problems.md`) or is called out below
as "engine" with what it emits today.

---

## Part 1: Confluence, manual changes after conversion

### 1. Reset-fields logic in RB_GroupTipo

Applies when the radio group `RB_GroupTipo` carries reset-fields logic. Keep only
the block marked `[configurator-reset-on-change]` and delete the rest of that
reset logic. The configurator generates its own reset-on-change block; extra
logic carried over from the source duplicates or contradicts it and can clear
fields that must keep their value.

Engine: `profiles/ubs/aem/custom/tipo_radio.xml` and the `reset_targets` branch
of `radiobutton.xml` emit exactly one Value Commit block, so an engine-authored
form has nothing to delete. The note applies to hand-built and legacy forms.

### 2. Renaming a panel requested by the repeating-panels sweep

When the sweep reports that a panel (`PN_...`) has to be renamed, rename it and
update every rule inside that panel to the new name. The sweep renames the panel
only. A rule that still names the old panel keeps looking correct in the editor
and never fires at runtime.

Check after renaming: rules on the panel itself; rules on child fields that use
an absolute path containing the old panel name; rules in other panels or on the
page that point into the renamed panel.

### 3. Jump-to-field (Edit) button on the first page

Applies when the first page contains no repeatable panels: add the jump-to-field
button to the page title by hand. On the first page the title panel sits after
the `PN_BR` panel holding the banking-relationship fragment, and the tooling
sometimes fails to recognise it as the element that carries the button, leaving
the page without one on the summary.

Does not apply when the first page does contain repeatable panels. There the
buttons belong to the repeatable panels, one per instance, and a button on the
title would show as a duplicate.

Checklist for the summary page, independent of the rule above:

- No Edit button on text-only pages.
- No Edit button on the form configurator page, even with no repeatable panels:
  the configurator is excluded from the summary.
- No Edit button on the title of a page that contains repeatable panels.
- One Edit button on every repeatable panel instance of such pages, nested
  repeatables included, not only the outermost. Signature panels count, since
  every signature is modelled as a repeatable panel.
- One Edit button on the title of a page with no repeatable panels but with
  fields that can be changed.
- Never two Edit buttons above one heading. A duplicate means the step panel and
  its title panel both carry it; remove the one on the step panel.
- A page with no dedicated title panel keeps the button on the step panel.
- An Edit button on a nested panel that is neither a step, a step title nor a
  repeatable is a known tooling issue, not something to fix in the form.

### 4. Checkbox groups must not be interrupted by other fields

Applies when the source shows a list of checkbox options with input fields
between them (an option, then the text field belonging to it, then the next
option).

1. Model the whole list as one checkbox component with several options, not one
   component per option.
2. Put every field linked to an option after that component, not between the
   options.
3. Wire each linked field's visibility to its option.

A checkbox component owns all of its options as one unit. Splitting it, or
inserting fields between the options, breaks the group: the data model stops
matching the source and the option-driven visibility rules cannot be expressed
cleanly.

### 5. Field of width 6 alone on a line

A field of width 6 that is the only field on its line gets its display set to 2
columns, otherwise it is not displayed correctly in the DoR.

### 6. Address fragment (affrg_AddressGeneric1)

Fragment path
`/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_AddressGeneric1`.

Italy forms usually need a reduced address block: street number, additional
details, postal code and city, state and district (APAC) hidden, and city and
country not mandatory. The rule paths depend on where the address block sits, so
there are two variants. Use exactly one.

#### 6.1 Inside the address fragment itself

Applies when editing `affrg_AddressGeneric1`, where `PN_AddressBlock` is a direct
child.

```javascript
window.forms.ubs.hideAFHideDor(this.PN_AddressBlock.TXT_StreetNumber);
window.forms.ubs.hideAFHideDor(this.PN_AddressBlock.TXT_AdditionalDetails_AddressBlock);
window.forms.ubs.hideAFHideDor(this.PN_AddressBlock.TXT_PostalCodeCity);
window.forms.ubs.hideAFHideDor(this.PN_AddressBlock.TXT_State_AddressBlock);
window.forms.ubs.hideAFHideDor(this.PN_AddressBlock.TXT_District_AddressBlock_APAC);
window.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.TXT_City_AddressBlock, false);
window.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.DD_Country_AddressBlock, false);
```

#### 6.2 In fragments that contain the address fragment

Applies when a fragment includes `afforms_ubs_fragmentlib/affrg_AddressGeneric1`
and the contained address fragment is not hidden. The address block is then
reached through the wrapping panel `PN_Address`, and a few panels of the host
fragment have to be hidden as well.

```javascript
window.forms.ubs.hideAFHideDor(this.PN_EntityBasic);
window.forms.ubs.hideAFHideDor(this.PN_FormAddress);
window.forms.ubs.hideAFHideDor(this.PN_DOBNationality);
window.forms.ubs.hideAFHideDor(this.PN_DateIncorporation);

window.forms.ubs.hideAFHideDor(this.PN_Address.PN_AddressBlock.TXT_StreetNumber);
window.forms.ubs.hideAFHideDor(this.PN_Address.PN_AddressBlock.TXT_AdditionalDetails_AddressBlock);
window.forms.ubs.hideAFHideDor(this.PN_Address.PN_AddressBlock.TXT_PostalCodeCity);
window.forms.ubs.hideAFHideDor(this.PN_Address.PN_AddressBlock.TXT_State_AddressBlock);
window.forms.ubs.hideAFHideDor(this.PN_Address.PN_AddressBlock.TXT_District_AddressBlock_APAC);

window.com.ajila.forms.ubs.components.setMandatory(this.PN_Address.PN_AddressBlock.TXT_City_AddressBlock, false);
window.com.ajila.forms.ubs.components.setMandatory(this.PN_Address.PN_AddressBlock.DD_Country_AddressBlock, false);
```

Notes:

- The only difference between the two blocks for the address fields is the path
  prefix: `this.PN_AddressBlock...` inside the fragment,
  `this.PN_Address.PN_AddressBlock...` from a fragment that embeds it.
- Variant 6.2 additionally hides `PN_EntityBasic`, `PN_FormAddress`,
  `PN_DOBNationality` and `PN_DateIncorporation`, which only exist in the host
  fragment.
- If the embedded address fragment is already hidden in the host fragment, do not
  add variant 6.2.
- `hideAFHideDor` hides the field in the form and in the DoR;
  `setMandatory(..., false)` only removes the mandatory flag.

Engine: `address_init` and `address_generic_init` in
`profiles/ubs/aem/config.toml` carry the `setMandatory` half (feedback #102). The
`hideAFHideDor` half of 6.1 is in `address_generic_init`; 6.2 is not emitted.

### 7. Text below the banking relationship

Many forms carry a text below the banking relationship. To reproduce it, create a
static text below the banking-relationship fragment with these settings, so it
appears in the header of the DoR: header slot assignment (slot 2), exclude from
summary page, show on DoR even if hidden on the adaptive form, show in the PDF
even if hidden on the summary page, and hide object.

Reference shape, from the owner's hand-built `AAOS_033` and `AAOV_033` packages
(second child of `PN_BR`, after the fragment panel):

```xml
<textdraw
    sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/textdraw"
    _value="&lt;p>&lt;b>UBS Europe SE&lt;/b> (Succursale Italia)&lt;/p>&#xa;"
    alwaysInPdf="true"
    dorFieldStyling="Default"
    dorHeaderSlot="slot2"
    guideNodeClass="guideTextDraw"
    showIfHidden="true"
    summaryExclusion="true"
    textIsRich="true"
    visible="{Boolean}false"/>
```

Engine: the document authors this draw itself, a hidden `TextDraw` with
`dor_header_slot` `slot2` right after the `Preface` (rule `ubs-aem-dor-header-slots`),
and the writer places it inside `PN_BR`.

---

## Part 2: Review notes, 2026-08-26

Grouped by what the note is about. The name at the end is who the note was
assigned to in the session.

### Titles and subtitles

- Subtitles use the `subtitle-after-form-title` class and are static text, not an
  `h2` ("Attestazione di avvenuta consegna"). Patrice.
- A subtitle panel must not carry a title of its own (Patrice's rule).
- Whether a subtitle is bold follows the source form.
- "Titolare del conto" rendering bold is caused by the wrong panel type: the UBS
  panel has to be used, not the default AEM panel.

### First page and banking relationship

- The address fragment always has to be present. Manual work.
- Save progress has to be added. Fabian.
- The UBS panel has to be used. Fabian.
- Add "UBS Europe SE" to slot 2, either in the existing subtitle panel or in the
  banking-relationship panel.
- Banking Relationship is missing everywhere and should not be excluded. Michi.

### Document of record

- The infobox belongs on the last page of the DoR: hidden on the adaptive form,
  shown in the footnotes of the last page. Fabian.
- Entire sections are hidden in the DoR. Whether that is a form issue or an
  environment issue is still open; investigate.
- Text is missing in the HTML preview. Michi.
- A checkbox renders wrongly in the DoR: a multiline caption must be aligned
  properly. Michi.
- Everything excluded from the DoR must also be excluded from the summary.
- Check every child panel for an "exclude from title" setting.
- FIM signature verification does not work: the block does not show in the DoR
  when the checkbox is enabled.
- Numbering is wrong: roman in the form, arabic in the DoR.
- The summary is wrong in AAOV.

### Repeatables and buttons

- With repeatable instance titles, the Edit button belongs on the title of the
  repeatable, and the Edit buttons come off the main page title, but only if that
  page holds nothing but the repeatable. Marco.
- "Firma": remove the jump-to-field button when every fragment on the page has a
  jump-to-field button of its own. Marco.
- The Remove button sits outside its panel. Optional.

### Layout

- Specific fields in the fragments keep a bad layout for now.
- Additional address details do not have to be hidden, though hiding them is not
  wrong.
- Checkboxes and radio buttons belong in one group, and any field between them
  moves below the group.
- When a field is hidden the layout goes wrong. How to proceed is unclear: the
  business wants a proper layout, but the fields have to be hidden.

### Comparison

- Compare against AABT, the form QA confirmed as working.
