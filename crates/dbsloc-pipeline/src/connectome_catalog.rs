// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Catalogue of the normative connectomes Lead-DBS users work with (`specs/15` §2.2, §4.1).

pub const REFERENCE_SPACE: &str = "MNI152NLin2009bAsym";
pub const LEADDBS_RELEASE_URL: &str = "https://www.lead-dbs.org/release/download.php";
pub const DATAVERSE_BASE_URL: &str = "https://dataverse.harvard.edu";
pub const FIGSHARE_BASE_URL: &str = "https://ndownloader.figshare.com/files";
pub const GSP_DOI: &str = "doi:10.7910/DVN/KKTJQC";

/// Lead-DBS folder class a connectome is installed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectomeClass {
    Dmri,
    Fmri,
    DmriMultiTract,
}

impl ConnectomeClass {
    pub const ALL: [ConnectomeClass; 3] = [
        ConnectomeClass::Dmri,
        ConnectomeClass::Fmri,
        ConnectomeClass::DmriMultiTract,
    ];

    /// Install subfolder name (`dMRI`, `fMRI`, `dMRI_MultiTract`).
    pub fn dir_name(self) -> &'static str {
        match self {
            ConnectomeClass::Dmri => "dMRI",
            ConnectomeClass::Fmri => "fMRI",
            ConnectomeClass::DmriMultiTract => "dMRI_MultiTract",
        }
    }

    pub fn parse(text: &str) -> Option<ConnectomeClass> {
        let t = text.trim();
        ConnectomeClass::ALL
            .into_iter()
            .find(|c| c.dir_name().eq_ignore_ascii_case(t))
    }

    /// `Structural` or `Functional`, as the cards label the class.
    pub fn kind_label(self) -> &'static str {
        match self {
            ConnectomeClass::Fmri => "Functional",
            _ => "Structural",
        }
    }
}

/// How a user may obtain a connectome (`specs/15` §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Open,
    CohortTerms,
    Dua,
}

impl Access {
    pub fn label(self) -> &'static str {
        match self {
            Access::Open => "open",
            Access::CohortTerms => "cohort_terms",
            Access::Dua => "dua",
        }
    }
}

/// Where one file of an entry is fetched from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    LeadDbs { id: &'static str },
    Dataverse { file_id: u64 },
    Figshare { file_id: u64 },
}

/// One numbered instruction of an import-only entry: a sentence and an optional link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub text: &'static str,
    pub url: Option<&'static str>,
}

/// How the downloaded files become the connectome folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    ZipFolder { folder: &'static str },
    GspTars { folder: &'static str },
}

/// One downloadable file with its pins; `source_digest` is `sha1:<hex>` or `md5:<hex>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectomeFile {
    pub source: Source,
    pub file_name: &'static str,
    pub bytes: u64,
    pub source_digest: &'static str,
    pub sha256: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectomeEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub class: ConnectomeClass,
    pub space: &'static str,
    pub cohort: &'static str,
    pub citation: &'static str,
    pub acknowledgement: &'static str,
    pub licence_name: &'static str,
    pub licence_url: &'static str,
    pub licence_text: &'static str,
    /// Date the verbatim terms were fetched from `licence_url`.
    pub terms_fetched: &'static str,
    pub access: Access,
    pub agreement_with: Option<&'static str>,
    pub dataset_url: &'static str,
    pub instructions: &'static str,
    pub steps: &'static [Step],
    pub files: &'static [ConnectomeFile],
    pub install: Install,
    pub folder_verified: bool,
    pub marker: &'static str,
    pub tag: Option<&'static str>,
    pub version: &'static str,
}

impl ConnectomeEntry {
    /// Name of the folder installed under `<root>/<class>/`.
    pub fn folder(&self) -> &'static str {
        match self.install {
            Install::ZipFolder { folder } | Install::GspTars { folder } => folder,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    /// The attestation sentence shown and recorded for a `dua` entry.
    pub fn attestation_sentence(&self) -> String {
        format!(
            "I hold an approved data use agreement with {} that covers this use.",
            self.agreement_with.unwrap_or(self.cohort)
        )
    }
}

const HORN_PAGE: &str = "https://www.lead-dbs.org/?p=7692";
const WUMINN_URL: &str = "https://www.humanconnectome.org/study/hcp-young-adult/document/wu-minn-hcp-consortium-open-access-data-use-terms";
const PPMI_URL: &str = "https://www.ppmi-info.org/access-data-specimens/download-data";

pub const TERMS_FETCHED: &str = "2026-10-23";

const TERMS_GIBBS: &str =
    include_str!("../../../resources/licenses/connectomes/horn2013-gibbs19.txt");
const TERMS_NKI: &str =
    include_str!("../../../resources/licenses/connectomes/horn2016-nkirs169.txt");
const TERMS_PEDIATRIC: &str =
    include_str!("../../../resources/licenses/connectomes/alfatly2023-pediatric100.txt");
const TERMS_HCP_MGH: &str =
    include_str!("../../../resources/licenses/connectomes/horn2017-mghusc-hcp32.txt");
const TERMS_PPMI: &str = include_str!("../../../resources/licenses/connectomes/ppmi.txt");
const TERMS_WUMINN: &str =
    include_str!("../../../resources/licenses/connectomes/elias2024-dtor985.txt");
const TERMS_GSP: &str =
    include_str!("../../../resources/licenses/connectomes/cohen2023-gsp1000v2.txt");
const TERMS_TORPD: &str =
    include_str!("../../../resources/licenses/connectomes/loh2020-torpd75.txt");

const PPMI_DUA_URL: &str =
    "https://www.ppmi-info.org/sites/default/files/docs/ppmi-data-use-agreement.pdf";

const OPEN_INSTRUCTIONS: &str = "Read the licence and citation, tick the acceptance box and download. The file comes from the original host; Wyss DBS Localizer never mirrors it.";

const TERMS_INSTRUCTIONS: &str = "Read the data use terms, tick the acceptance box and download. The file comes from the original host; Wyss DBS Localizer never mirrors it.";

const PPMI_INSTRUCTIONS: &str = "Import only: get access from PPMI, download the archive yourself and import it; the numbered steps are below.";

const HCP_INSTRUCTIONS: &str = "Import only: get access from the MGH-USC Human Connectome Project, download the archive yourself and import it; the numbered steps are below.";

const LEADDBS_CONNECTOMES_PAGE: &str =
    "https://www.lead-dbs.org/helpsupport/knowledge-base/atlasesresources/normative-connectomes/";
const PPMI_PUBLICATION_POLICY_URL: &str =
    "https://www.ppmi-info.org/sites/default/files/docs/ppmi-publication-policy.pdf";
const HCP_AGREEMENT_URL: &str = "https://www.humanconnectome.org/storage/app/media/documentation/mgh_diffusion/HCP_Data_Agreement.pdf";

const IMPORT_STEP: &str = "Tick both boxes below, choose \u{201c}Import archive\u{2026}\u{201d} and pick the downloaded file. If Lead-DBS already installed this connectome on this computer, choose \u{201c}Choose folder\u{2026}\u{201d} and pick its folder instead.";

macro_rules! ppmi_steps {
    ($file:expr) => {
        &[
            Step {
                text: "Read the PPMI Data Use Agreement.",
                url: Some(PPMI_DUA_URL),
            },
            Step {
                text: "Create an account at the LONI Image and Data Archive and submit the PPMI application. PPMI states that applications are reviewed within one week.",
                url: Some("https://ida.loni.usc.edu/collaboration/access/appApply.jsp?project=PPMI"),
            },
            Step {
                text: concat!(
                    "After PPMI approved your application, download the archive ",
                    $file,
                    " in your web browser from the Lead-DBS Normative connectomes page."
                ),
                url: Some(LEADDBS_CONNECTOMES_PAGE),
            },
            Step {
                text: IMPORT_STEP,
                url: None,
            },
            Step {
                text: "Follow the PPMI Publication Policy and use the acknowledgement shown below in publications.",
                url: Some(PPMI_PUBLICATION_POLICY_URL),
            },
        ]
    };
}

static HCP_STEPS: [Step; 5] = [
    Step {
        text: "Read the HCP Data Use Agreement of the MGH-USC Adult Diffusion data.",
        url: Some(HCP_AGREEMENT_URL),
    },
    Step {
        text: "Create an account at the LONI Image and Data Archive, submit the HCP application and accept the agreement there.",
        url: Some("https://ida.loni.usc.edu/collaboration/access/appApply.jsp?project=HCP"),
    },
    Step {
        text: "After your access is confirmed, download the archive groupconnectome_HCP_MGH_32fold_horn_2017.zip in your web browser from the Lead-DBS Normative connectomes page.",
        url: Some(LEADDBS_CONNECTOMES_PAGE),
    },
    Step {
        text: IMPORT_STEP,
        url: None,
    },
    Step {
        text: "Use the acknowledgement shown below in publications.",
        url: None,
    },
];

const CIT_HORN2013: &str = "Horn A, Ostwald D, Reisert M, Blankenburg F. The structural-functional connectome and the default mode network of the human brain. NeuroImage 102 (2014) 142-151.";
const CIT_HORN2016: &str = "Horn A, Blankenburg F. Toward a standardized structural-functional group connectome in MNI space. NeuroImage 124 (2016) 310-322.";
const CIT_HORN2017: &str = "Horn A, Kuhn AA, Merkl A, Shih L, Alterman R, Fox M. Probabilistic conversion of neurosurgical DBS electrode coordinates into MNI space. NeuroImage 150 (2017) 395-404. Setsompop K, et al. Pushing the limits of in vivo diffusion MRI for the Human Connectome Project. NeuroImage 80 (2013) 220-233.";
const CIT_EWERT: &str = "Ewert S, et al. Toward defining deep brain stimulation targets in MNI space: a subcortical atlas based on multimodal MRI, histology and structural connectivity. NeuroImage 170 (2018) 271-282. Marek K, et al. The Parkinson Progression Marker Initiative (PPMI). Prog Neurobiol 95 (2011) 629-635.";
const CIT_PPMI_FMRI: &str = "Horn A, Reich M, Vorwerk J, et al. Connectivity predicts deep brain stimulation outcome in Parkinson disease. Ann Neurol 82 (2017) 67-78. Marek K, et al. The Parkinson Progression Marker Initiative (PPMI). Prog Neurobiol 95 (2011) 629-635.";
const CIT_DTOR: &str = "Elias GJB, et al. A large normative connectome for exploring the tractographic correlates of focal brain interventions. Sci Data 11 (2024), doi:10.1038/s41597-024-03197-0; data: figshare doi:10.6084/m9.figshare.c.6844890.";
const CIT_TORPD: &str = "Loh A, Boutet A, et al. A functional connectome of Parkinson's disease patients prior to deep brain stimulation: a tool for disease-specific connectivity analyses. Front Neurosci (2022); data: Zenodo doi:10.5281/zenodo.4310183.";
const CIT_PEDIATRIC: &str = "Al-Fatly B, et al. NeuroImage: Clinical (2023), doi:10.1016/j.nicl.2023.103449 (pediatric functional connectome).";
const CIT_GSP: &str = "Cohen A, Al-Fatly B, Horn A. GSP1000 Preprocessed Connectome for Lead DBS. Harvard Dataverse (2023), doi:10.7910/DVN/KKTJQC. Holmes AJ, et al. Brain Genomics Superstruct Project initial data release. Sci Data 2 (2015) 150031.";

const ACK_CITE: &str = "Cite the publication above when you use this connectome.";
const ACK_HCP: &str = "Item 11: Data collection and sharing for this project was provided by the Human Connectome Project (HCP; Principal Investigators: Bruce Rosen, M.D., Ph.D., Arthur W. Toga, Ph.D., Van J. Weeden, MD). HCP funding was provided by the National Institute of Dental and Craniofacial Research (NIDCR), the National Institute of Mental Health (NIMH), and the National Institute of Neurological Disorders and Stroke (NINDS). HCP data are disseminated by the Laboratory of Neuro Imaging at the University of Southern California.\nItem 12 (methods section, some variation is mandatory): \"Data used in the preparation of this work were obtained from the Human Connectome Project (HCP) database (https://ida.loni.usc.edu/login.jsp). The HCP project (Principal Investigators: Bruce Rosen, M.D., Ph.D., Martinos Center at Massachusetts General Hospital; Arthur W. Toga, Ph.D., University of Southern California, Van J. Weeden, MD, Martinos Center at Massachusetts General Hospital) is supported by the National Institute of Dental and Craniofacial Research (NIDCR), the National Institute of Mental Health (NIMH) and the National Institute of Neurological Disorders and Stroke (NINDS). HCP is the result of efforts of co-investigators from the University of Southern California, Martinos Center for Biomedical Imaging at Massachusetts General Hospital (MGH), Washington University, and the University of Minnesota.\"";
const ACK_PPMI: &str = "Data used in the preparation of this article was obtained on <download date> from the Parkinson\u{2019}s Progression Markers Initiative (PPMI) database (www.ppmi-info.org/access-data-specimens/download-data), RRID:SCR_006431. For up-to-date information on the study, visit www.ppmi-info.org. PPMI \u{2013} a public-private partnership \u{2013} is funded by the Michael J. Fox Foundation for Parkinson\u{2019}s Research and funding partners, including AbbVie, Alamar Biosciences, Aligning Science Across Parkinson\u{2019}s (ASAP), Arrowhead Pharma, Arvinas, AskBio, BIAL, BioArctic, Biohaven, BlueRock Therapeutics, Bristol Myers Squibb, Calico Labs, Capsida Biotherapeutics, Critical Path Institute, DaCapo Brainscience, Denali, Edmond J. Safra Foundation, Eli Lilly, Gain Therapeutics, GE Healthcare, Genentech, GSK, Insitro, Johnson & Johnson Innovative Medicine, Lundbeck, Merck, Neumora, Neuron23, Novartis, Olink, Regeneron, Roche, Sanofi, Tenvie, UCB, Vanqua Bio, Voyager Therapeutics, The Weston Family Foundation.";
const ACK_WUMINN: &str = "Data were provided [in part] by the Human Connectome Project, WU-Minn Consortium (Principal Investigators: David Van Essen and Kamil Ugurbil; 1U54MH091657) funded by the 16 NIH Institutes and Centers that support the NIH Blueprint for Neuroscience Research; and by the McDonnell Center for Systems Neuroscience at Washington University.";
const ACK_GSP: &str = "Data were provided [in part] by the Brain Genomics Superstruct Project of Harvard University and the Massachusetts General Hospital, (Principal Investigators: Randy Buckner, Joshua Roffman, and Jordan Smoller), with support from the Center for Brain Science Neuroinformatics Research Group, the Athinoula A. Martinos Center for Biomedical Imaging, and the Center for Human Genetic Research. 20 individual investigators at Harvard and MGH generously contributed data to the overall project.";
const ACK_CORR: &str = "Please cite the consortium paper, and where applicable the site specific paper (if available). Consortium paper: Zuo X.N., et al., An Open Science Resource for Establishing Reliability and Reproducibility in Functional Connectomics. Sci Data 1 (2014) 140049. Dataset DOI NYU 2: http://dx.doi.org/10.15387/fcp_indi.corr.nyu2. Also cite Al-Fatly B, et al. NeuroImage: Clinical (2023), doi:10.1016/j.nicl.2023.103449.";
const ACK_TORPD: &str = "Loh, Aaron; Boutet, Alexandre; Germann, Jurgen; Al-Fatly, Bassam; Elias, Gavin J.B.; Neudorfer, Clemens; Krotz, Jillian; Wong, Emily H.Y.; Parmar, Roohie; Gramer, Robert; Paff, Michelle; Horn, Andreas; Chen, J. Jean; Azevedo, Paula; Fasano, Alfonso; Munhoz, Renato P.; Hodaie, Mojgan; Kalia, Suneil K.; Kucharczyk, Walter; Lozano, Andres M. (2020). A disease-specific functional connectome of Parkinson's disease patients. Zenodo. https://doi.org/10.5281/zenodo.4310183";

macro_rules! lead_file {
    ($id:expr, $file:expr, $bytes:expr, $sha1:expr) => {
        lead_file!($id, $file, $bytes, $sha1, "")
    };
    ($id:expr, $file:expr, $bytes:expr, $sha1:expr, $sha256:expr) => {
        &[ConnectomeFile {
            source: Source::LeadDbs { id: $id },
            file_name: $file,
            bytes: $bytes,
            source_digest: concat!("sha1:", $sha1),
            sha256: $sha256,
        }]
    };
}

macro_rules! figshare_file {
    ($file_id:expr, $file:expr, $bytes:expr, $md5:expr, $sha256:expr) => {
        &[ConnectomeFile {
            source: Source::Figshare { file_id: $file_id },
            file_name: $file,
            bytes: $bytes,
            source_digest: concat!("md5:", $md5),
            sha256: $sha256,
        }]
    };
}

const DTOR_COLLECTION_URL: &str = "https://doi.org/10.6084/m9.figshare.c.6844890";

static CATALOGUE: [ConnectomeEntry; 11] = [
    ConnectomeEntry {
        id: "horn2013-gibbs19",
        name: "Global Tracking 19 (Horn 2013) Full",
        class: ConnectomeClass::Dmri,
        space: REFERENCE_SPACE,
        cohort: "19 healthy subjects (Gibbs tracking)",
        citation: CIT_HORN2013,
        acknowledgement: ACK_CITE,
        licence_name: "Downloadable without restriction (Data in Brief)",
        licence_url: "https://doi.org/10.1016/j.dib.2015.08.035",
        licence_text: TERMS_GIBBS,
        terms_fetched: TERMS_FETCHED,
        access: Access::Open,
        agreement_with: None,
        dataset_url: HORN_PAGE,
        instructions: OPEN_INSTRUCTIONS,
        steps: &[],
        files: &[ConnectomeFile {
            source: Source::LeadDbs { id: "group2013" },
            file_name: "groupconnectome_horn_2013.zip",
            bytes: 3_015_912_949,
            source_digest: "sha1:f738d3e7d02c8e0b9a525bfd4f1ceb9adb7af2a1",
            sha256: "0895deb030b6e9cc2a15e71fa7f021507cd1494586fa99bba6f9f60fb573962e",
        }],
        install: Install::ZipFolder {
            folder: "Global Tracking 19 (Horn 2013) Full",
        },
        folder_verified: true,
        marker: "data.mat",
        tag: None,
        version: "1.0",
    },
    ConnectomeEntry {
        id: "horn2016-nkirs169",
        name: "NKI-RS 169 (Horn 2016)",
        class: ConnectomeClass::Dmri,
        space: REFERENCE_SPACE,
        cohort: "NKI Rockland Sample, 169 subjects (Gibbs tracking)",
        citation: CIT_HORN2016,
        acknowledgement: ACK_CITE,
        licence_name: "Non-commercial research use (INDI policy); NITRC release listed as GPL-2",
        licence_url: "https://fcon_1000.projects.nitrc.org/indi/pro/eNKI_RS_TRT/NKI_Rockland_nifti_dl.html",
        licence_text: TERMS_NKI,
        terms_fetched: TERMS_FETCHED,
        access: Access::Open,
        agreement_with: None,
        dataset_url: "http://rocklandsample.org",
        instructions: "Read the licence and citation, tick the acceptance box and download. This connectome is for non-commercial research use. The file comes from the original host; Wyss DBS Localizer never mirrors it.",
        steps: &[],
        files: lead_file!(
            "group2016",
            "groupconnectome_nki_169_horn_2016.zip",
            7_325_698_796,
            "170e793e2f7cf27247bb41e5c1842b8d82b38940",
            "5c4e69be4a653c9221378346fc5277f327ba031f1a01f86068960b177b45f61b"
        ),
        install: Install::ZipFolder {
            folder: "NKI-RS 169 (Horn 2016)",
        },
        folder_verified: false,
        marker: "data.mat",
        tag: None,
        version: "1.0",
    },
    ConnectomeEntry {
        id: "horn2017-mghusc-hcp32",
        name: "MGH-USC HCP 32 (Horn 2017)",
        class: ConnectomeClass::Dmri,
        space: REFERENCE_SPACE,
        cohort: "HCP MGH-USC Adult Diffusion, 32 subjects (GQI)",
        citation: CIT_HORN2017,
        acknowledgement: ACK_HCP,
        licence_name: "MGH-USC HCP Data Use Agreement (2014)",
        licence_url: "https://www.humanconnectome.org/storage/app/media/documentation/mgh_diffusion/HCP_Data_Agreement.pdf",
        licence_text: TERMS_HCP_MGH,
        terms_fetched: TERMS_FETCHED,
        access: Access::Dua,
        agreement_with: Some("the MGH-USC Human Connectome Project"),
        dataset_url: "https://ida.loni.usc.edu/",
        instructions: HCP_INSTRUCTIONS,
        steps: &HCP_STEPS,
        files: lead_file!(
            "group2017",
            "groupconnectome_HCP_MGH_32fold_horn_2017.zip",
            4_289_260_330,
            "422eb83ccf4e540b27e23873524b6a60d0e41ef7"
        ),
        install: Install::ZipFolder {
            folder: "MGH-USC HCP 32 (Horn 2017)",
        },
        folder_verified: true,
        marker: "data.mat",
        tag: Some("MGHUSCHCP32"),
        version: "1.2",
    },
    ConnectomeEntry {
        id: "ewert2017-ppmi85",
        name: "PPMI 85 (Ewert 2017)",
        class: ConnectomeClass::Dmri,
        space: REFERENCE_SPACE,
        cohort: "PPMI Parkinson patients, 85 subjects (GQI)",
        citation: CIT_EWERT,
        acknowledgement: ACK_PPMI,
        licence_name: "PPMI Data Use Agreement, Version 5.0 (April 2026)",
        licence_url: PPMI_DUA_URL,
        licence_text: TERMS_PPMI,
        terms_fetched: TERMS_FETCHED,
        access: Access::Dua,
        agreement_with: Some("the Parkinson's Progression Markers Initiative (PPMI)"),
        dataset_url: PPMI_URL,
        instructions: PPMI_INSTRUCTIONS,
        steps: ppmi_steps!("groupconnectome_ppmi_85_ewert_2017.zip"),
        files: lead_file!(
            "group2017_ppmi",
            "groupconnectome_ppmi_85_ewert_2017.zip",
            685_944_962,
            "ae94d8f0d8b58c83202abde75de5c4c5e0bc1cd7"
        ),
        install: Install::ZipFolder {
            folder: "PPMI 85 (Ewert 2017)",
        },
        folder_verified: false,
        marker: "data.mat",
        tag: None,
        version: "1.1",
    },
    ConnectomeEntry {
        id: "elias2024-dtor985-quarter",
        name: "dTOR-985 Quarter",
        class: ConnectomeClass::Dmri,
        space: REFERENCE_SPACE,
        cohort: "HCP WU-Minn young adults, 985 subjects (GQI), quarter of the streamlines",
        citation: CIT_DTOR,
        acknowledgement: ACK_WUMINN,
        licence_name: "CC0 1.0 and WU-Minn HCP Open Access Data Use Terms",
        licence_url: WUMINN_URL,
        licence_text: TERMS_WUMINN,
        terms_fetched: TERMS_FETCHED,
        access: Access::CohortTerms,
        agreement_with: None,
        dataset_url: DTOR_COLLECTION_URL,
        instructions: TERMS_INSTRUCTIONS,
        steps: &[],
        files: figshare_file!(
            52596914,
            "dTOR-985_Quarter.zip",
            2_886_958_128,
            "c75d3044f82ab68d1268263f8e7ff7c0",
            "dfac8ca9673707ffd396b4a8ef48028a79041610711d8b569e3308967c4cb34a"
        ),
        install: Install::ZipFolder {
            folder: "dTOR-985 Quarter",
        },
        folder_verified: false,
        marker: "data.mat",
        tag: None,
        version: "figshare v2",
    },
    ConnectomeEntry {
        id: "elias2024-dtor985-half",
        name: "dTOR-985 Half",
        class: ConnectomeClass::Dmri,
        space: REFERENCE_SPACE,
        cohort: "HCP WU-Minn young adults, 985 subjects (GQI), half of the streamlines",
        citation: CIT_DTOR,
        acknowledgement: ACK_WUMINN,
        licence_name: "CC0 1.0 and WU-Minn HCP Open Access Data Use Terms",
        licence_url: WUMINN_URL,
        licence_text: TERMS_WUMINN,
        terms_fetched: TERMS_FETCHED,
        access: Access::CohortTerms,
        agreement_with: None,
        dataset_url: DTOR_COLLECTION_URL,
        instructions: TERMS_INSTRUCTIONS,
        steps: &[],
        files: figshare_file!(
            52597346,
            "dTOR-985_Half.zip",
            5_776_636_849,
            "97ca0b18ea4fbfeb27192964efdcb1f5",
            "d50ca0896b12842b6176905e7e7f19aeeffc7eb999d504c0d2e46b7120aea986"
        ),
        install: Install::ZipFolder {
            folder: "dTOR-985 Half",
        },
        folder_verified: false,
        marker: "data.mat",
        tag: None,
        version: "figshare v2",
    },
    ConnectomeEntry {
        id: "elias2024-dtor985-full",
        name: "dTOR-985 Full",
        class: ConnectomeClass::Dmri,
        space: REFERENCE_SPACE,
        cohort: "HCP WU-Minn young adults, 985 subjects (GQI), all streamlines",
        citation: CIT_DTOR,
        acknowledgement: ACK_WUMINN,
        licence_name: "CC0 1.0 and WU-Minn HCP Open Access Data Use Terms",
        licence_url: WUMINN_URL,
        licence_text: TERMS_WUMINN,
        terms_fetched: TERMS_FETCHED,
        access: Access::CohortTerms,
        agreement_with: None,
        dataset_url: DTOR_COLLECTION_URL,
        instructions: TERMS_INSTRUCTIONS,
        steps: &[],
        files: figshare_file!(
            52597988,
            "dTOR-985_Full.zip",
            11_551_215_059,
            "576841ad68eb01e8ed2c9b4a59608fbf",
            "c244c127d0e9d98926586259d9cce74e89d0d57760494751f50e61f60765a132"
        ),
        install: Install::ZipFolder {
            folder: "dTOR-985 Full",
        },
        folder_verified: false,
        marker: "data.mat",
        tag: None,
        version: "figshare v2",
    },
    ConnectomeEntry {
        id: "horn2017-ppmi74p15c",
        name: "PPMI 74 Patients 15 Controls (Horn 2017)",
        class: ConnectomeClass::Fmri,
        space: REFERENCE_SPACE,
        cohort: "PPMI, 89 subjects (74 patients, 15 controls), 2 mm",
        citation: CIT_PPMI_FMRI,
        acknowledgement: ACK_PPMI,
        licence_name: "PPMI Data Use Agreement, Version 5.0 (April 2026)",
        licence_url: PPMI_DUA_URL,
        licence_text: TERMS_PPMI,
        terms_fetched: TERMS_FETCHED,
        access: Access::Dua,
        agreement_with: Some("the Parkinson's Progression Markers Initiative (PPMI)"),
        dataset_url: PPMI_URL,
        instructions: PPMI_INSTRUCTIONS,
        steps: ppmi_steps!("functional_groupconnectome_ppmi_74_15_horn_2017.zip"),
        files: lead_file!(
            "fgroup2017_ppmi",
            "functional_groupconnectome_ppmi_74_15_horn_2017.zip",
            8_042_457_187,
            "5c7b578ca2bfb693b42b6feb3b218ef7cd1bb43d"
        ),
        install: Install::ZipFolder {
            folder: "PPMI 74 Patients 15 Controls (Horn 2017)",
        },
        folder_verified: true,
        marker: "dataset_info.json",
        tag: Some("PPMI74P15C"),
        version: "1.0",
    },
    ConnectomeEntry {
        id: "loh2020-torpd75",
        name: "Tor PD (Loh & Boutet 2020)",
        class: ConnectomeClass::Fmri,
        space: REFERENCE_SPACE,
        cohort: "Toronto Parkinson patients, 75 subjects (77 acquisitions), 2 mm",
        citation: CIT_TORPD,
        acknowledgement: ACK_TORPD,
        licence_name: "CC BY 4.0",
        licence_url: "https://creativecommons.org/licenses/by/4.0/",
        licence_text: TERMS_TORPD,
        terms_fetched: TERMS_FETCHED,
        access: Access::Open,
        agreement_with: None,
        dataset_url: "https://doi.org/10.5281/zenodo.4310183",
        instructions: OPEN_INSTRUCTIONS,
        steps: &[],
        files: lead_file!(
            "TorPD",
            "TorPD_Loh_Boutet_2020.zip",
            25_665_221_855,
            "13768da336ade8e6d3b92a229bafc289d9d3aa59",
            "b3c48089b6ed10908ff8a582e906bfb97dd6cdd75cb8629027585f17c9b7787f"
        ),
        install: Install::ZipFolder {
            folder: "Tor PD (Loh & Boutet 2020)",
        },
        folder_verified: false,
        marker: "dataset_info.json",
        tag: None,
        version: "1.0",
    },
    ConnectomeEntry {
        id: "alfatly2023-pediatric100",
        name: "Pediatric rs-fMRI",
        class: ConnectomeClass::Fmri,
        space: "MNI pediatric template",
        cohort: "CoRR NYU 2, 100 children aged 6 to 18 years",
        citation: CIT_PEDIATRIC,
        acknowledgement: ACK_CORR,
        licence_name: "No licence (Lead-DBS maintainers, 2026-11-11); INDI / CoRR access policy, non-commercial; source cohort to be confirmed",
        licence_url: "https://fcon_1000.projects.nitrc.org/indi/req_access.html",
        licence_text: TERMS_PEDIATRIC,
        terms_fetched: TERMS_FETCHED,
        access: Access::Open,
        agreement_with: None,
        dataset_url: HORN_PAGE,
        instructions: "Read the licence and citation, tick the acceptance box and download. This connectome is for non-commercial research use; source cohort CoRR (INDI), so check the CoRR terms of use before you publish with it. The file comes from the original host; Wyss DBS Localizer never mirrors it.",
        steps: &[],
        files: lead_file!(
            "PedrsfMRI",
            "Pediatric_rsfMRI.zip",
            17_008_253_598,
            "987c3a92f5a8c46c312c2c07ed467788c3a721b9",
            "9ffb784deca21a3ca54e7d48aed16949a81a752f38b54ba8dc221ff6f50231a9"
        ),
        install: Install::ZipFolder {
            folder: "Pediatric rs-fMRI",
        },
        folder_verified: false,
        marker: "dataset_info.json",
        tag: None,
        version: "1.0",
    },
    ConnectomeEntry {
        id: "cohen2023-gsp1000v2",
        name: "GSP 1000 v2 (Cohen 2023)",
        class: ConnectomeClass::Fmri,
        space: REFERENCE_SPACE,
        cohort: "Brain Genomics Superstruct Project, 1000 subjects, 2 mm",
        citation: CIT_GSP,
        acknowledgement: ACK_GSP,
        licence_name: "GSP Open Access Data Use Terms (2014-Apr-22)",
        licence_url: "https://doi.org/10.7910/DVN/KKTJQC",
        licence_text: TERMS_GSP,
        terms_fetched: TERMS_FETCHED,
        access: Access::CohortTerms,
        agreement_with: None,
        dataset_url: "https://doi.org/10.7910/DVN/KKTJQC",
        instructions: TERMS_INSTRUCTIONS,
        steps: &[],
        files: &GSP_FILES,
        install: Install::GspTars {
            folder: "GSP 1000 v2 (Cohen 2023)",
        },
        folder_verified: true,
        marker: "dataset_info.json",
        tag: Some("GSP1000v2"),
        version: "Dataverse 4.0",
    },
];

pub fn catalogue() -> &'static [ConnectomeEntry] {
    &CATALOGUE
}

/// Entry by id (exact, after trimming).
pub fn lookup(id: &str) -> Option<&'static ConnectomeEntry> {
    let key = id.trim();
    CATALOGUE.iter().find(|e| e.id == key)
}

/// The entry an installed folder matches: folder name equal to the entry's folder or name, or a
/// `dataset_info.json` tag equal to the entry's tag (`specs/15` §3.1).
pub fn match_installed(
    class: ConnectomeClass,
    folder_name: &str,
    tag: Option<&str>,
) -> Option<&'static ConnectomeEntry> {
    CATALOGUE.iter().find(|e| {
        e.class == class
            && (e.folder() == folder_name
                || e.name == folder_name
                || matches!((e.tag, tag), (Some(a), Some(b)) if a == b))
    })
}

static GSP_FILES: [ConnectomeFile; 38] = [
    ConnectomeFile {
        source: Source::Dataverse { file_id: 11539066 },
        file_name: "dataset_info.json",
        bytes: 4733,
        source_digest: "md5:ff8ce4749fcdb74b79dcc4b75d83320e",
        sha256: "e11d215fdccbd19f2e0f4219fc6617eba4fd649916927782e109589c48ff4ca2",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 11058539 },
        file_name: "dataset_info.mat",
        bytes: 2228896,
        source_digest: "md5:3e8f5aefeae5e68cd015dc11fccd83bd",
        sha256: "457cd11192048f7760defaddcb3b5925a2a2cf580910e77e2318164ae2fefb6e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 11058538 },
        file_name: "dataset_volsurf.mat",
        bytes: 2210520,
        source_digest: "md5:452c75d43d0bd6c5ddf5683ed35024d3",
        sha256: "0dcc556615a6950972b4a786aa6d5acb0167e86ec779fad3493df5b4ee5aef80",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7611704 },
        file_name: "GSP1000_leadDBS_00.tar",
        bytes: 6142536704,
        source_digest: "md5:5877590a6dcb9b5879368f9702deac9e",
        sha256: "8d778a2f59cc7d6b0175dbacebba2d39042420add8bfcd80a377551be3bb28df",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7611708 },
        file_name: "GSP1000_leadDBS_01.tar",
        bytes: 6269285376,
        source_digest: "md5:929f54b3403312397e261a56fc71429a",
        sha256: "b0b1664c1008bab3fdfc1a55ad1fb5aa7b480c23d214329f041febd84441b499",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7611709 },
        file_name: "GSP1000_leadDBS_02.tar",
        bytes: 6269563392,
        source_digest: "md5:a3780effda50da654dff8fc5c0a65965",
        sha256: "aea2f1db7aedad967f56703069f2f06b1dc9fefc901ef8abc82e75b4c0c7df5e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612743 },
        file_name: "GSP1000_leadDBS_03.tar",
        bytes: 6270397440,
        source_digest: "md5:60da1d99090c56e1f5668dbfbe191bbd",
        sha256: "c1ed2da5cfc492c1412616b76e796057a99e3629eafdced1845c4356ab25cc7e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612744 },
        file_name: "GSP1000_leadDBS_04.tar",
        bytes: 6269744128,
        source_digest: "md5:3014d3afaaa2d782cb5e778b659eb7ce",
        sha256: "c7d183e2feab292e293cc771e619015603625e717cf72df8a97a98ad45b33672",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612746 },
        file_name: "GSP1000_leadDBS_05.tar",
        bytes: 6269561344,
        source_digest: "md5:006413c1e15c58014c1cd11433a0c78c",
        sha256: "eab284da8518a34da981a5b78032f72a0efdca3e1c8b5d6fd7db424a3e9e710a",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612747 },
        file_name: "GSP1000_leadDBS_06.tar",
        bytes: 6268104704,
        source_digest: "md5:f71cb449562f0346d7bb11b2df568508",
        sha256: "924249d9203ec8d1c65fe05358471b7ff570ee6520bd5c52af9de9acdb9e0da2",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612756 },
        file_name: "GSP1000_leadDBS_07.tar",
        bytes: 6269067264,
        source_digest: "md5:30c12c5e5c26de530c1a983e0d4a38b7",
        sha256: "dc320a0f92c8f9af7e03d7a55f68229f9432c3df6fe6cc5f87c48b4c9ea1f21a",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612817 },
        file_name: "GSP1000_leadDBS_08.tar",
        bytes: 6268318720,
        source_digest: "md5:4a01d1421303def467f0dc796d71b0ed",
        sha256: "ebca3ffd09513e8d322c9bffc4c178edc5cc7e324f9da8320f77d73561de2e6e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612844 },
        file_name: "GSP1000_leadDBS_09.tar",
        bytes: 6269735936,
        source_digest: "md5:856493a997f5f42db2f055e28b3776cc",
        sha256: "179b5682ad4c8007d45d7222af22cbd0d63b1715364dcae0d215e42b140cf9de",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612913 },
        file_name: "GSP1000_leadDBS_10.tar",
        bytes: 6270212608,
        source_digest: "md5:337bb859769216ffb14c6da86ebfd675",
        sha256: "481c6ebbfdcc753fbc3203ce37eb185a68435d95ece0d9f4940d51721410fc02",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612914 },
        file_name: "GSP1000_leadDBS_11.tar",
        bytes: 6269484544,
        source_digest: "md5:0f9e0d0e93bbb1a534fb93bf5db8d1d5",
        sha256: "4f00f8d2008488345f21a7fb191b6328db577e0b21265b24690e6040c9c73113",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612917 },
        file_name: "GSP1000_leadDBS_12.tar",
        bytes: 6269851136,
        source_digest: "md5:97ebf1b420fb32d9b13c3c42d7d011f2",
        sha256: "aa1edb2998032e1c239e85c1e91c250d5dc33ff96acecf219aa993ec0306a05a",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612961 },
        file_name: "GSP1000_leadDBS_13.tar",
        bytes: 6269012480,
        source_digest: "md5:5b578cc761008f43d4f366b75fdac800",
        sha256: "ef95a20cff48a3868236f66a451da63badafecd56558fba28fe487e6a6d7d027",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612968 },
        file_name: "GSP1000_leadDBS_14.tar",
        bytes: 6269215232,
        source_digest: "md5:4b60d1efb50966a3a0b403e87044dcff",
        sha256: "6cff8b57b4e15818cbf72773e1bf48db3249af61d486554926a520e576d9c388",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612971 },
        file_name: "GSP1000_leadDBS_15.tar",
        bytes: 6270123520,
        source_digest: "md5:1399ff6bb1112a2ca1334beca02f7a92",
        sha256: "d6aede2f5e2e59800ddd8430dc92318392111a6ff2b4199b23ca5d8f444ee70d",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7612973 },
        file_name: "GSP1000_leadDBS_16.tar",
        bytes: 6269361664,
        source_digest: "md5:7ec037f3c541743834f5323706300849",
        sha256: "efa55c9a6c16cc62ad02c696a8af6505bf6fcabc1a80bb50157d853f308932e4",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613009 },
        file_name: "GSP1000_leadDBS_17.tar",
        bytes: 6271344640,
        source_digest: "md5:37810cd1ea212e65d1fad4380bd04ad8",
        sha256: "e26d0201e3d125aac1779f01e25f98ae15fe68428c5953591bf64ab51a90c6c6",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613097 },
        file_name: "GSP1000_leadDBS_18.tar",
        bytes: 6269627904,
        source_digest: "md5:6036e2df1271f05679608f87bd1e50db",
        sha256: "6765293b95ff8152e75850ee60329986c369ac32d8f3fd4096a51c40e1aa8a2e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613167 },
        file_name: "GSP1000_leadDBS_19.tar",
        bytes: 6269739520,
        source_digest: "md5:9404bc1103f1f23ea4fe93bde2e79e82",
        sha256: "a0f17640ea6619c27880bf0d96fd319e8e02ca66de9877112021b94354bf229e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613346 },
        file_name: "GSP1000_leadDBS_20.tar",
        bytes: 6268455936,
        source_digest: "md5:c64012363af589789d630d0474f50876",
        sha256: "4d7945cab3bb65f50eae94e53b422ab997de3317ae0f1ea36057443ec84bb71e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613672 },
        file_name: "GSP1000_leadDBS_21.tar",
        bytes: 6269213184,
        source_digest: "md5:277cbcf3d5972c03ac59070beee14f17",
        sha256: "58334ba2ff58d23ea0d9c1775d67371f4fb8a9e8f04bc9ba285a70661080a6b9",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613740 },
        file_name: "GSP1000_leadDBS_22.tar",
        bytes: 6269089792,
        source_digest: "md5:211443b7202dbbe0325845d74bb78dbc",
        sha256: "bdeb1777878a5f4c4f2e8351757110c617f12de8e034c08bc6fb02b984de11be",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613804 },
        file_name: "GSP1000_leadDBS_23.tar",
        bytes: 6269406720,
        source_digest: "md5:d37b187ff1a327cfa34d5d69e7a07ab1",
        sha256: "a9511949ce1542bd58cd8d19e05d564de264a38b23bf62ff36b5fb14d1a69fd1",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613897 },
        file_name: "GSP1000_leadDBS_24.tar",
        bytes: 6269859840,
        source_digest: "md5:4b5bf38885faa83fddd52916b2fb692b",
        sha256: "4111a64d6db64faab1731e813d244b0402a6884e781675a0b01f635254e07da1",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7613968 },
        file_name: "GSP1000_leadDBS_25.tar",
        bytes: 6269807616,
        source_digest: "md5:96e341d4e7d2a93054432021d82674fb",
        sha256: "c73516134515dc72ce7b7f4c1ba841042fc7e6a42df50e4e0159f9c589b1e853",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7615347 },
        file_name: "GSP1000_leadDBS_26.tar",
        bytes: 6269944832,
        source_digest: "md5:6aeb586fcbcbd0b80cb778ac6c425f37",
        sha256: "d9fc6acd03a25fb32a94555ed76ce9bf6def7055d671b2c86672a2ec7445fd1f",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614038 },
        file_name: "GSP1000_leadDBS_27.tar",
        bytes: 6269011456,
        source_digest: "md5:959f7e97373f4525b8497a606c51c200",
        sha256: "28f10d3dfde20a18f29a42245fb57e02a0a721a18f8ca0eb759219bb754e217c",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614101 },
        file_name: "GSP1000_leadDBS_28.tar",
        bytes: 6268562432,
        source_digest: "md5:2146e58ef7246a322f243140966afc17",
        sha256: "ee8b97bc2d1273f29ea2acfa7d7bf7c1cd9d1965c927d11eb82a2e4e843888dc",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614181 },
        file_name: "GSP1000_leadDBS_29.tar",
        bytes: 6268871168,
        source_digest: "md5:21373ebb941ff9d4d3ef7fc3129575ad",
        sha256: "cff3616b6934e74e6b8b6da7af3b781a5001139cd2d0cc7a82d40d717e611a6e",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614259 },
        file_name: "GSP1000_leadDBS_30.tar",
        bytes: 6270990336,
        source_digest: "md5:7289338addfb96a97baeeb7952bd2c89",
        sha256: "c7913b3ca87a7eafba453e30a5976bd91d4a4f148af2af7670af8d6f7e654cfe",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614309 },
        file_name: "GSP1000_leadDBS_31.tar",
        bytes: 6268421120,
        source_digest: "md5:c46428d65253e9621cff24d13c76aa20",
        sha256: "b2adfa0e6dcb98e3475776320fa4c4d8d5fa8db3eae5c7f055c23896c469443d",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614378 },
        file_name: "GSP1000_leadDBS_32.tar",
        bytes: 6269055488,
        source_digest: "md5:10c65ff4a1641d9891758bd23f53be69",
        sha256: "802b3ea789bd3dded2d485cce507aad42cf854d2b4606a16af06d7f3fce82cc7",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614459 },
        file_name: "GSP1000_leadDBS_33.tar",
        bytes: 6269453312,
        source_digest: "md5:91baead8259295060a85a48f76762a57",
        sha256: "97d930525eb2325c8ed2a9bc9fbd4e3c65e4363c7b91db5f43353372f4c642fd",
    },
    ConnectomeFile {
        source: Source::Dataverse { file_id: 7614525 },
        file_name: "GSP1000_leadDBS_34.tar",
        bytes: 5893967360,
        source_digest: "md5:1eec3e560c942af1aa3beb11af92437a",
        sha256: "b354ae55d692aaeea0733d916446b02271adff3b1c4df4a908033478f1351fcc",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eleven_entries_with_unique_ids() {
        let ids: std::collections::BTreeSet<&str> = catalogue().iter().map(|e| e.id).collect();
        assert_eq!(catalogue().len(), 11);
        assert_eq!(ids.len(), 11);
    }

    #[test]
    fn every_entry_has_the_mandatory_fields() {
        for e in catalogue() {
            assert!(!e.name.is_empty() && !e.cohort.is_empty(), "{}", e.id);
            assert!(
                !e.citation.is_empty() && !e.acknowledgement.is_empty(),
                "{}",
                e.id
            );
            assert!(
                !e.licence_name.is_empty() && !e.licence_text.is_empty(),
                "{}",
                e.id
            );
            assert!(
                !e.licence_url.is_empty() && !e.instructions.is_empty(),
                "{}",
                e.id
            );
            assert!(!e.marker.is_empty() && !e.version.is_empty(), "{}", e.id);
            assert!(!e.files.is_empty(), "{}", e.id);
            for f in e.files {
                assert!(f.bytes > 0, "{} {}", e.id, f.file_name);
                let (kind, hex) = f.source_digest.split_once(':').unwrap_or_default();
                let want = match kind {
                    "sha1" => 40,
                    "md5" => 32,
                    _ => 0,
                };
                assert!(want > 0 && hex.len() == want, "{} {}", e.id, f.file_name);
                assert!(hex
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
            }
            if e.access == Access::Dua {
                assert!(e.dataset_url.starts_with("http"), "{}", e.id);
                assert!(e.agreement_with.is_some(), "{}", e.id);
            }
        }
    }

    #[test]
    fn access_classes_follow_the_spec_table() {
        let access = |id: &str| lookup(id).map(|e| e.access);
        assert_eq!(access("horn2017-mghusc-hcp32"), Some(Access::Dua));
        assert_eq!(access("ewert2017-ppmi85"), Some(Access::Dua));
        assert_eq!(access("horn2017-ppmi74p15c"), Some(Access::Dua));
        assert_eq!(access("elias2024-dtor985-full"), Some(Access::CohortTerms));
        assert_eq!(access("cohen2023-gsp1000v2"), Some(Access::CohortTerms));
        assert_eq!(access("loh2020-torpd75"), Some(Access::Open));
        assert_eq!(access("horn2013-gibbs19"), Some(Access::Open));
    }

    #[test]
    fn import_only_entries_carry_numbered_steps_with_https_links() {
        for e in catalogue() {
            if e.access == Access::Dua {
                assert!(e.steps.len() >= 4, "{}", e.id);
                for s in e.steps {
                    assert!(!s.text.is_empty(), "{}", e.id);
                    if let Some(url) = s.url {
                        assert!(url.starts_with("https://"), "{} {url}", e.id);
                    }
                }
                assert!(e.steps.iter().any(|s| s.url.is_some()), "{}", e.id);
                for f in e.files {
                    assert!(
                        e.steps.iter().any(|s| s.text.contains(f.file_name)),
                        "{} {}",
                        e.id,
                        f.file_name
                    );
                }
            } else {
                assert!(e.steps.is_empty(), "{}", e.id);
            }
        }
    }

    #[test]
    fn dtor_entries_come_from_figshare_with_md5_digests() {
        let want = [
            (
                "elias2024-dtor985-quarter",
                52596914,
                "c75d3044f82ab68d1268263f8e7ff7c0",
            ),
            (
                "elias2024-dtor985-half",
                52597346,
                "97ca0b18ea4fbfeb27192964efdcb1f5",
            ),
            (
                "elias2024-dtor985-full",
                52597988,
                "576841ad68eb01e8ed2c9b4a59608fbf",
            ),
        ];
        for (id, file_id, md5) in want {
            let e = lookup(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(e.files.len(), 1, "{id}");
            assert_eq!(e.files[0].source, Source::Figshare { file_id }, "{id}");
            assert_eq!(e.files[0].source_digest, format!("md5:{md5}"), "{id}");
            assert_eq!(
                e.dataset_url,
                "https://doi.org/10.6084/m9.figshare.c.6844890"
            );
            assert_eq!(e.access, Access::CohortTerms);
        }
    }

    #[test]
    fn pinned_sha256_values_are_lowercase_hex() {
        let gibbs = lookup("horn2013-gibbs19").unwrap_or_else(|| panic!("missing"));
        assert_eq!(
            gibbs.files[0].sha256,
            "0895deb030b6e9cc2a15e71fa7f021507cd1494586fa99bba6f9f60fb573962e"
        );
        for e in catalogue().iter().filter(|e| e.access != Access::Dua) {
            for f in e.files {
                assert!(!f.sha256.is_empty(), "{} {}", e.id, f.file_name);
            }
        }
        for e in catalogue() {
            for f in e.files {
                if !f.sha256.is_empty() {
                    assert_eq!(f.sha256.len(), 64, "{} {}", e.id, f.file_name);
                    assert!(
                        f.sha256
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                        "{} {}",
                        e.id,
                        f.file_name
                    );
                }
            }
        }
    }

    #[test]
    fn indi_derived_entries_stay_open_with_the_non_commercial_notice() {
        let nki = lookup("horn2016-nkirs169").unwrap_or_else(|| panic!("missing"));
        let ped = lookup("alfatly2023-pediatric100").unwrap_or_else(|| panic!("missing"));
        assert_eq!(nki.access, Access::Open);
        assert_eq!(ped.access, Access::Open);
        assert!(nki.instructions.contains("for non-commercial research use"));
        assert!(ped.instructions.contains("for non-commercial research use"));
        assert!(ped.instructions.contains("source cohort CoRR (INDI)"));
    }

    #[test]
    fn gsp_has_38_files_summing_to_the_pinned_total() {
        let e = lookup("cohen2023-gsp1000v2").unwrap_or_else(|| panic!("missing"));
        assert_eq!(e.files.len(), 38);
        assert_eq!(e.total_bytes(), 218_932_842_997);
        assert_eq!(
            e.files
                .iter()
                .filter(|f| f.file_name.ends_with(".tar"))
                .count(),
            35
        );
        assert!(e
            .files
            .iter()
            .all(|f| matches!(f.source, Source::Dataverse { .. })));
    }

    #[test]
    fn horn_sizes_match_the_spec() {
        let sizes: Vec<u64> = catalogue()
            .iter()
            .map(ConnectomeEntry::total_bytes)
            .collect();
        assert_eq!(sizes[0], 3_015_912_949);
        assert_eq!(sizes[2], 4_289_260_330);
        assert_eq!(sizes[6], 11_551_215_059);
        assert_eq!(sizes[9], 17_008_253_598);
    }

    #[test]
    fn classes_parse_and_name_their_folders() {
        assert_eq!(
            ConnectomeClass::parse(" fmri "),
            Some(ConnectomeClass::Fmri)
        );
        assert_eq!(
            ConnectomeClass::parse("dMRI_MultiTract").map(ConnectomeClass::dir_name),
            Some("dMRI_MultiTract")
        );
        assert_eq!(ConnectomeClass::parse("x"), None);
    }

    #[test]
    fn installed_folders_match_by_folder_or_tag() {
        let by_folder = match_installed(ConnectomeClass::Dmri, "MGH-USC HCP 32 (Horn 2017)", None);
        assert_eq!(by_folder.map(|e| e.id), Some("horn2017-mghusc-hcp32"));
        let by_tag = match_installed(ConnectomeClass::Fmri, "renamed", Some("GSP1000v2"));
        assert_eq!(by_tag.map(|e| e.id), Some("cohen2023-gsp1000v2"));
        assert!(match_installed(ConnectomeClass::Dmri, "renamed", Some("GSP1000v2")).is_none());
    }

    #[test]
    fn attestation_names_the_agreement_holder() {
        let e = lookup("ewert2017-ppmi85").unwrap_or_else(|| panic!("missing"));
        assert!(e.attestation_sentence().contains("PPMI"));
    }

    #[test]
    fn every_entry_carries_the_verbatim_terms_with_their_provenance() {
        for e in catalogue() {
            let first = e.licence_text.lines().next().unwrap_or("");
            assert!(first.starts_with("catalogue_ids:"), "{}", e.id);
            assert!(
                e.licence_text
                    .lines()
                    .any(|l| l.starts_with("source_url: ") && l.len() > "source_url: ".len()),
                "{}",
                e.id
            );
            assert!(e.licence_text.len() > 500, "{}", e.id);
            assert!(
                e.licence_text.contains(e.id),
                "{} not named in its terms file",
                e.id
            );
            assert!(e.licence_url.starts_with("http"), "{}", e.id);
            assert_eq!(e.terms_fetched, "2026-10-23", "{}", e.id);
            let fetched = format!("fetched: {}", e.terms_fetched);
            assert!(e.licence_text.lines().any(|l| l == fetched), "{}", e.id);
        }
    }

    #[test]
    fn terms_files_carry_the_required_wording() {
        let get = |id: &str| lookup(id).unwrap_or_else(|| panic!("missing {id}"));
        let hcp = get("horn2017-mghusc-hcp32");
        assert!(hcp.acknowledgement.contains("Item 11"));
        assert!(hcp.acknowledgement.contains("Item 12"));
        let ppmi = get("ewert2017-ppmi85").acknowledgement;
        assert!(ppmi.contains("<download date>") && ppmi.contains("RRID:SCR_006431"));
        assert!(ppmi.contains("Voyager Therapeutics") && ppmi.contains("The Weston Family"));
        assert!(get("elias2024-dtor985-full")
            .acknowledgement
            .contains("[in part]"));
        let gsp = get("cohen2023-gsp1000v2");
        assert!(gsp.acknowledgement.contains("[in part]"));
        assert!(gsp.acknowledgement.contains("20 individual investigators"));
        assert!(!gsp.licence_text.contains('\u{e2}'));
        assert!(get("horn2016-nkirs169").licence_name.contains("GPL-2"));
        assert!(get("horn2013-gibbs19")
            .licence_name
            .contains("without restriction"));
        assert!(get("alfatly2023-pediatric100")
            .licence_name
            .contains("to be confirmed"));
    }
}
