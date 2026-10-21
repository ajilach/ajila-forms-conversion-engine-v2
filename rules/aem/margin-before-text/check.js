// The fragments of the bank's libraries whose last item, as they open, is an input. Derived from
// the profile's fragment content (`profiles/ubs/aem/fragments`, following nested fragments and
// skipping hidden items); agent/tests/rules.rs derives it again and fails when the two differ.
const ENDS_IN_INPUT = [
  "afforms_ch_fragmentlib/affrg_AHSignature1",
  "afforms_ch_fragmentlib/affrg_AHSignatureDepot1",
  "afforms_ch_fragmentlib/affrg_ARPOASignature1",
  "afforms_ch_fragmentlib/affrg_ARSignature1",
  "afforms_ch_fragmentlib/affrg_ARSignature2",
  "afforms_ch_fragmentlib/affrg_AccountHolder1",
  "afforms_ch_fragmentlib/affrg_AccountHolderBasic1",
  "afforms_ch_fragmentlib/affrg_AccountHolderBasic2",
  "afforms_ch_fragmentlib/affrg_AccountHolderDepot1",
  "afforms_ch_fragmentlib/affrg_Address1",
  "afforms_ch_fragmentlib/affrg_Applicant1",
  "afforms_ch_fragmentlib/affrg_Attorney1",
  "afforms_ch_fragmentlib/affrg_AttySignature1",
  "afforms_ch_fragmentlib/affrg_AuthRep1",
  "afforms_ch_fragmentlib/affrg_AuthSigner1",
  "afforms_ch_fragmentlib/affrg_BOSignature1",
  "afforms_ch_fragmentlib/affrg_BRSignature1",
  "afforms_ch_fragmentlib/affrg_Bank1",
  "afforms_ch_fragmentlib/affrg_BeneficialOwner1",
  "afforms_ch_fragmentlib/affrg_BeneficialOwner2",
  "afforms_ch_fragmentlib/affrg_Beneficiary1",
  "afforms_ch_fragmentlib/affrg_CA1",
  "afforms_ch_fragmentlib/affrg_CAAssistantSignature1",
  "afforms_ch_fragmentlib/affrg_CASignature1",
  "afforms_ch_fragmentlib/affrg_Client1",
  "afforms_ch_fragmentlib/affrg_ClientBasic1",
  "afforms_ch_fragmentlib/affrg_ClientBasic2",
  "afforms_ch_fragmentlib/affrg_ClientPOAHolderSignature1",
  "afforms_ch_fragmentlib/affrg_ClientSignature1",
  "afforms_ch_fragmentlib/affrg_ContractingPartner1",
  "afforms_ch_fragmentlib/affrg_ContractingPartnerSignature1",
  "afforms_ch_fragmentlib/affrg_ContractingPartnetBasic1",
  "afforms_ch_fragmentlib/affrg_ControllingPerson1",
  "afforms_ch_fragmentlib/affrg_ControllingPerson2",
  "afforms_ch_fragmentlib/affrg_DatauserSignature1",
  "afforms_ch_fragmentlib/affrg_DeskHeadSignature1",
  "afforms_ch_fragmentlib/affrg_EndClientSignature1",
  "afforms_ch_fragmentlib/affrg_FIM1",
  "afforms_ch_fragmentlib/affrg_FIMSignature1",
  "afforms_ch_fragmentlib/affrg_GenericSignature1",
  "afforms_ch_fragmentlib/affrg_IndividualBasic1",
  "afforms_ch_fragmentlib/affrg_IndividualBasic2",
  "afforms_ch_fragmentlib/affrg_Payee1",
  "afforms_ch_fragmentlib/affrg_PensionHSignature1",
  "afforms_ch_fragmentlib/affrg_Pledgor1",
  "afforms_ch_fragmentlib/affrg_Principal1",
  "afforms_ch_fragmentlib/affrg_PrincipalSignature1",
  "afforms_ch_fragmentlib/affrg_Recipient1",
  "afforms_ch_fragmentlib/affrg_RelevantPSignature1",
  "afforms_ch_fragmentlib/affrg_RelevantPerson1",
  "afforms_ch_fragmentlib/affrg_Signature-Pledgor",
  "afforms_germany_fragmentlib/affrg_ARPerson_1",
  "afforms_germany_fragmentlib/affrg_ARSignature1",
  "afforms_germany_fragmentlib/affrg_Address1",
  "afforms_germany_fragmentlib/affrg_AuthRep",
  "afforms_germany_fragmentlib/affrg_AuthRep1",
  "afforms_germany_fragmentlib/affrg_AuthorizedRepresentative_Info",
  "afforms_germany_fragmentlib/affrg_AuthorizedSignature_Frag",
  "afforms_germany_fragmentlib/affrg_Authorized_Person1",
  "afforms_germany_fragmentlib/affrg_BOSignature1",
  "afforms_germany_fragmentlib/affrg_ClientBasic1",
  "afforms_germany_fragmentlib/affrg_ClientSignature1",
  "afforms_germany_fragmentlib/affrg_DepositorBasic_1",
  "afforms_germany_fragmentlib/affrg_DepositorSignature1",
  "afforms_germany_fragmentlib/affrg_EntityBasic1",
  "afforms_germany_fragmentlib/affrg_EntityBasic2",
  "afforms_germany_fragmentlib/affrg_IndividualBasic1",
  "afforms_germany_fragmentlib/affrg_LegalGuardianSignature1",
  "afforms_germany_fragmentlib/affrg_Sign_authorizedsignatory",
  "afforms_germany_fragmentlib/affrg_Signature_Account_Holder",
  "afforms_germany_fragmentlib/affrg_UBSEuropeSignature1",
  "afforms_germany_fragmentlib/affrg_addressclientid",
  "afforms_germany_fragmentlib/affrg_german_ClientAdvisorSignature_Signature_Place_Date_Name",
  "afforms_germany_fragmentlib/affrg_germany_AddressBlock_CountryDD",
  "afforms_germany_fragmentlib/affrg_germany_BankingRelationship",
  "afforms_germany_fragmentlib/affrg_germany_Client_Signature",
  "afforms_germany_fragmentlib/affrg_germany_Form_Address",
  "afforms_germany_fragmentlib/affrg_germany_PersonalDetails_CompanyName",
  "afforms_germany_fragmentlib/affrg_germany_PersonalDetails_Indvidual_Company",
  "afforms_germany_fragmentlib/affrg_germany_PersonalDetails_Name",
  "afforms_italy_fragmentlib/affrg_Address1",
  "afforms_italy_fragmentlib/affrg_ClientBasic1",
  "afforms_italy_fragmentlib/affrg_ClientSignature1",
  "afforms_italy_fragmentlib/affrg_ClienteRepeatingPanel",
  "afforms_italy_fragmentlib/affrg_EntityBasic1",
  "afforms_italy_fragmentlib/affrg_EntityBasic2",
  "afforms_italy_fragmentlib/affrg_IndividualBasic1",
  "afforms_italy_fragmentlib/affrg_LegalGuardianSignature1",
  "afforms_italy_fragmentlib/affrg_LegalRepreseRepeating",
  "afforms_italy_fragmentlib/affrg_LegalRepresentative",
  "afforms_italy_fragmentlib/affrg_LegalRepresentativeSignature1",
  "afforms_italy_fragmentlib/affrg_Sign_authorizedsignatory",
  "afforms_italy_fragmentlib/affrg_UBSEuropeSignature1",
  "afforms_italy_fragmentlib/affrg_accountHolderSignature",
  "afforms_italy_fragmentlib/affrg_addressclientid",
  "afforms_italy_fragmentlib/affrg_italiy_BankingRelationship",
  "afforms_italy_fragmentlib/affrg_italy_AddressBlock_CountryDD",
  "afforms_italy_fragmentlib/affrg_italy_ClientAdvisorSignature_Signature_Place_Date_Name",
  "afforms_italy_fragmentlib/affrg_italy_Client_Signature",
  "afforms_italy_fragmentlib/affrg_italy_Form_Address",
  "afforms_italy_fragmentlib/affrg_italy_PersonalDetails_CompanyName",
  "afforms_italy_fragmentlib/affrg_italy_PersonalDetails_Indvidual_Company",
  "afforms_italy_fragmentlib/affrg_italy_PersonalDetails_Name",
  "afforms_italy_fragmentlib/affrg_italy_personal_details",
  "afforms_italy_fragmentlib/affrg_signatureLegalRepresen",
  "afforms_ubs_fragmentlib/affrg_Account1",
  "afforms_ubs_fragmentlib/affrg_AddressGeneric1",
  "afforms_ubs_fragmentlib/affrg_BankingRelationship1",
  "afforms_ubs_fragmentlib/affrg_BeneficialOwnerGeneric1",
  "afforms_ubs_fragmentlib/affrg_BeneficialOwnerGenericBasic1",
  "afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1",
  "afforms_ubs_fragmentlib/affrg_ContractualPartnerGenericBasic1",
  "afforms_ubs_fragmentlib/affrg_CustodyAccount1",
  "afforms_ubs_fragmentlib/affrg_DOBandNationality",
  "afforms_ubs_fragmentlib/affrg_DateandCountryIncorporation",
  "afforms_ubs_fragmentlib/affrg_EntityBasic1",
  "afforms_ubs_fragmentlib/affrg_EntityBasic2",
  "afforms_ubs_fragmentlib/affrg_FormofAddress",
  "afforms_ubs_fragmentlib/affrg_IBAN1",
  "afforms_ubs_fragmentlib/affrg_IBAN2",
  "afforms_ubs_fragmentlib/affrg_IndividualBasic1",
  "afforms_ubs_fragmentlib/affrg_PartnertoPartnerGeneric1",
  "afforms_ubs_fragmentlib/affrg_PartnertoPartnerGenericBasic1",
  "afforms_ubs_fragmentlib/affrg_PowerofAttorneyGeneric1",
  "afforms_ubs_fragmentlib/affrg_PowerofAttorneyGenericBasic1",
  "afforms_ubs_fragmentlib/affrg_Product1",
  "afforms_ubs_fragmentlib/affrg_SafeDepositBox1",
  "afforms_ubs_fragmentlib/affrg_SafekepingAccount1",
  "afforms_ubs_fragmentlib/affrg_SignatureGeneric1"
];

const MARGIN = "ubs-margin-20";
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
const TEXTS = ["TextDraw", "TitleDraw", "HtmlDisplayer", "MessageBox"];

// `<library>/<fragment>` of a fragment path, as ENDS_IN_INPUT lists it.
function libraryEntry(fragRef) {
  const parts = String(fragRef).replace(/\/+$/, "").split("/");
  return parts.slice(-2).join("/");
}

// Whether `node` ends, as it renders, in an input.
function endsInInput(node) {
  if (INPUTS.indexOf(node.type) >= 0) return true;
  if (node.type === "Fragment") return ENDS_IN_INPUT.indexOf(libraryEntry(node.frag_ref)) >= 0;
  if (node.type === "Panel" || node.type === "Repeatable") {
    const children = (node.children || []).filter(function (c) { return c.type !== "FootnotePlaceholder"; });
    return children.length > 0 && endsInInput(children[children.length - 1]);
  }
  return false;
}

// Whether `node` starts, as it renders, with a text or heading.
function startsWithText(node) {
  if (TEXTS.indexOf(node.type) >= 0) return true;
  if (node.type === "Panel") {
    const children = node.children || [];
    return children.length > 0 && startsWithText(children[0]);
  }
  return false;
}

function hasMargin(node) {
  return String(node.css || "").split(/\s+/).indexOf(MARGIN) >= 0;
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const children = node.children || [];
    // The pages under the root are wizard steps, each a screen of its own.
    const pairs = node.type === "Root" ? 0 : children.length - 1;
    for (let i = 0; i < pairs; i++) {
      const before = children[i];
      if (endsInInput(before) && startsWithText(children[i + 1]) && !hasMargin(before)) {
        const at = pointer + "/children/" + i;
        violations.push({
          pointer: Object.prototype.hasOwnProperty.call(before, "css") ? at + "/css" : at,
          message: "`" + before.name + "` ends in an input and a text or heading follows it: add `" +
            MARGIN + "` to its css",
        });
      }
    }
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
