//! dlopen/dlsym binding to the system libxml2, resolved once (ICU-lane
//! precedent: no build-time dep; pods only guarantee the runtime .so).
//! Unloadable/missing symbol = the C "built without libxml" ERROR (0A000)
//! with a HINT naming the host package, never a silent no-libxml fallback.
//! `status()` reports the probe for `pgrust_runtime_status()`.

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

use core::ffi::{c_char, c_int, c_long, c_void, CStr};
use ::types_error::{PgError, ERRCODE_FEATURE_NOT_SUPPORTED};
use std::sync::OnceLock;

pub type c_uchar = u8;

pub enum xmlDoc {}
pub enum xmlParserCtxt {}
pub enum xmlBuffer {}
pub enum xmlTextWriter {}
pub enum xmlSaveCtxt {}
pub enum xmlXPathContext {}
pub enum xmlXPathCompExpr {}
pub enum xmlXPathObject {}
pub enum xmlNode {}
pub enum xmlNodeSet {}

pub const XML_ELEMENT_NODE: c_int = 1;
pub const XML_ATTRIBUTE_NODE: c_int = 2;
pub const XML_TEXT_NODE: c_int = 3;
pub const XML_DOCUMENT_NODE: c_int = 9;
// upstream 4c777d6dd9c9 (18.6): Fix handling of namespace nodes in xpath() (xml)
pub const XML_NAMESPACE_DECL: c_int = 18;

pub const XPATH_NODESET: c_int = 1;
pub const XPATH_BOOLEAN: c_int = 2;
pub const XPATH_NUMBER: c_int = 3;
pub const XPATH_STRING: c_int = 4;

pub const XML_PARSE_NOENT: c_int = 1 << 1;
pub const XML_PARSE_DTDATTR: c_int = 1 << 3;
pub const XML_PARSE_NOBLANKS: c_int = 1 << 8;
pub const XML_PARSE_NONET: c_int = 1 << 11;

pub const XML_SAVE_FORMAT: c_int = 1 << 0;
pub const XML_SAVE_NO_DECL: c_int = 1 << 1;

pub type xmlExternalEntityLoader =
    unsafe extern "C" fn(URL: *const c_char, ID: *const c_char, ctxt: *mut c_void) -> *mut c_void;
pub type xmlStructuredErrorFunc =
    unsafe extern "C" fn(user_data: *mut c_void, error: *mut c_void);
type xmlFreeFunc = unsafe extern "C" fn(mem: *mut c_void);

// Prefixes of libxml2's public structs through the fields xml.c reads;
// stable across libxml2 2.x — the same ABI contract C's xml.c relies on.
#[repr(C)]
pub struct xmlNodeHdr {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *const c_uchar,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlNode,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
}

#[repr(C)]
pub struct xmlDocHdr {
    pub _private: *mut c_void,
    pub type_: c_int,
    pub name: *const c_char,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlNode,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub compression: c_int,
    pub standalone: c_int,
    pub int_subset: *mut c_void,
    pub ext_subset: *mut c_void,
    pub old_ns: *mut c_void,
    pub version: *const c_uchar,
    pub encoding: *const c_uchar,
}

#[repr(C)]
pub struct xmlNodeSetHdr {
    pub node_nr: c_int,
    pub node_max: c_int,
    pub node_tab: *mut *mut xmlNode,
}

#[repr(C)]
pub struct xmlXPathObjectHdr {
    pub type_: c_int,
    pub nodesetval: *mut xmlNodeSet,
    pub boolval: c_int,
    pub floatval: f64,
    pub stringval: *mut c_uchar,
}

#[repr(C)]
pub struct xmlXPathContextHdr {
    pub doc: *mut xmlDoc,
    pub node: *mut xmlNode,
}

#[repr(C)]
pub struct xmlErrorHdr {
    pub domain: c_int,
    pub code: c_int,
    pub message: *mut c_char,
    pub level: c_int,
    pub file: *mut c_char,
    pub line: c_int,
    pub str1: *mut c_char,
    pub str2: *mut c_char,
    pub str3: *mut c_char,
    pub int1: c_int,
    pub int2: c_int,
    pub ctxt: *mut c_void,
    pub node: *mut c_void,
}

#[repr(C)]
pub struct xmlParserCtxtHdr {
    pub sax: *mut c_void,
    pub user_data: *mut c_void,
    pub my_doc: *mut c_void,
    pub well_formed: c_int,
    pub replace_entities: c_int,
    pub version: *const c_uchar,
    pub encoding: *const c_uchar,
    pub standalone: c_int,
    pub html: c_int,
    pub input: *mut xmlParserInputHdr,
}

#[repr(C)]
pub struct xmlParserInputHdr {
    pub buf: *mut c_void,
    pub filename: *const c_char,
    pub directory: *const c_char,
    pub base: *const c_uchar,
    pub cur: *const c_uchar,
    pub end: *const c_uchar,
    pub length: c_int,
    pub line: c_int,
    pub col: c_int,
}

pub struct LibXml2 {
    /// The candidate name that dlopen resolved (`CANDIDATES`).
    pub library: &'static str,
    /// libxml2's own `xmlParserVersion` ("21502" = 2.15.2), if exported.
    pub version: Option<String>,
    pub xmlInitParser: unsafe extern "C" fn(),
    xmlFreeVar: *const xmlFreeFunc,
    pub xmlStrdup: unsafe extern "C" fn(cur: *const c_uchar) -> *mut c_uchar,
    pub xmlStrlen: unsafe extern "C" fn(s: *const c_uchar) -> c_int,
    pub xmlNewParserCtxt: unsafe extern "C" fn() -> *mut xmlParserCtxt,
    pub xmlFreeParserCtxt: unsafe extern "C" fn(ctxt: *mut xmlParserCtxt),
    pub xmlCtxtReadDoc: unsafe extern "C" fn(
        ctxt: *mut xmlParserCtxt,
        cur: *const c_uchar,
        URL: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlDoc,
    pub xmlCtxtReadMemory: unsafe extern "C" fn(
        ctxt: *mut xmlParserCtxt,
        buffer: *const c_char,
        size: c_int,
        URL: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlDoc,
    pub xmlNewDoc: unsafe extern "C" fn(version: *const c_uchar) -> *mut xmlDoc,
    pub xmlFreeDoc: unsafe extern "C" fn(doc: *mut xmlDoc),
    pub xmlParseBalancedChunkMemory: unsafe extern "C" fn(
        doc: *mut xmlDoc,
        sax: *mut c_void,
        user_data: *mut c_void,
        depth: c_int,
        string: *const c_uchar,
        lst: *mut *mut xmlNode,
    ) -> c_int,
    pub xmlKeepBlanksDefault: unsafe extern "C" fn(val: c_int) -> c_int,
    pub xmlDocSetRootElement:
        unsafe extern "C" fn(doc: *mut xmlDoc, root: *mut xmlNode) -> *mut xmlNode,
    pub xmlNewNode:
        unsafe extern "C" fn(ns: *mut c_void, name: *const c_uchar) -> *mut xmlNode,
    pub xmlNewDocText:
        unsafe extern "C" fn(doc: *mut xmlDoc, content: *const c_uchar) -> *mut xmlNode,
    pub xmlAddChildList:
        unsafe extern "C" fn(parent: *mut xmlNode, cur: *mut xmlNode) -> *mut xmlNode,
    pub xmlFreeNode: unsafe extern "C" fn(node: *mut xmlNode),
    pub xmlBufferCreate: unsafe extern "C" fn() -> *mut xmlBuffer,
    pub xmlBufferFree: unsafe extern "C" fn(buf: *mut xmlBuffer),
    pub xmlBufferContent: unsafe extern "C" fn(buf: *const xmlBuffer) -> *const c_uchar,
    pub xmlBufferLength: unsafe extern "C" fn(buf: *const xmlBuffer) -> c_int,
    pub xmlSaveToBuffer: unsafe extern "C" fn(
        buffer: *mut xmlBuffer,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlSaveCtxt,
    pub xmlSaveDoc: unsafe extern "C" fn(ctxt: *mut xmlSaveCtxt, doc: *mut xmlDoc) -> c_long,
    pub xmlSaveTree: unsafe extern "C" fn(ctxt: *mut xmlSaveCtxt, node: *mut xmlNode) -> c_long,
    pub xmlSaveClose: unsafe extern "C" fn(ctxt: *mut xmlSaveCtxt) -> c_int,
    pub xmlNodeDump: unsafe extern "C" fn(
        buf: *mut xmlBuffer,
        doc: *mut xmlDoc,
        cur: *mut xmlNode,
        level: c_int,
        format: c_int,
    ) -> c_int,
    pub xmlCopyNode: unsafe extern "C" fn(node: *mut xmlNode, extended: c_int) -> *mut xmlNode,
    pub xmlNewTextWriterMemory:
        unsafe extern "C" fn(buf: *mut xmlBuffer, compression: c_int) -> *mut xmlTextWriter,
    pub xmlFreeTextWriter: unsafe extern "C" fn(writer: *mut xmlTextWriter),
    pub xmlTextWriterStartElement:
        unsafe extern "C" fn(writer: *mut xmlTextWriter, name: *const c_uchar) -> c_int,
    pub xmlTextWriterEndElement: unsafe extern "C" fn(writer: *mut xmlTextWriter) -> c_int,
    pub xmlTextWriterWriteAttribute: unsafe extern "C" fn(
        writer: *mut xmlTextWriter,
        name: *const c_uchar,
        content: *const c_uchar,
    ) -> c_int,
    pub xmlTextWriterWriteRaw:
        unsafe extern "C" fn(writer: *mut xmlTextWriter, content: *const c_uchar) -> c_int,
    pub xmlTextWriterWriteBase64: unsafe extern "C" fn(
        writer: *mut xmlTextWriter,
        data: *const c_char,
        start: c_int,
        len: c_int,
    ) -> c_int,
    pub xmlTextWriterWriteBinHex: unsafe extern "C" fn(
        writer: *mut xmlTextWriter,
        data: *const c_char,
        start: c_int,
        len: c_int,
    ) -> c_int,
    pub xmlEncodeSpecialChars:
        unsafe extern "C" fn(doc: *const xmlDoc, input: *const c_uchar) -> *mut c_uchar,
    pub xmlXPathNewContext: unsafe extern "C" fn(doc: *mut xmlDoc) -> *mut xmlXPathContext,
    pub xmlXPathFreeContext: unsafe extern "C" fn(ctxt: *mut xmlXPathContext),
    pub xmlXPathRegisterNs: unsafe extern "C" fn(
        ctxt: *mut xmlXPathContext,
        prefix: *const c_uchar,
        ns_uri: *const c_uchar,
    ) -> c_int,
    pub xmlXPathCtxtCompile: unsafe extern "C" fn(
        ctxt: *mut xmlXPathContext,
        expr: *const c_uchar,
    ) -> *mut xmlXPathCompExpr,
    pub xmlXPathFreeCompExpr: unsafe extern "C" fn(comp: *mut xmlXPathCompExpr),
    pub xmlXPathCompiledEval: unsafe extern "C" fn(
        comp: *mut xmlXPathCompExpr,
        ctxt: *mut xmlXPathContext,
    ) -> *mut xmlXPathObject,
    pub xmlXPathFreeObject: unsafe extern "C" fn(obj: *mut xmlXPathObject),
    pub xmlXPathCastNodeToString: unsafe extern "C" fn(node: *mut xmlNode) -> *mut c_uchar,
    pub xmlXPathCastNodeSetToString: unsafe extern "C" fn(ns: *mut xmlNodeSet) -> *mut c_uchar,
    pub xmlXPathCastBooleanToString: unsafe extern "C" fn(val: c_int) -> *mut c_uchar,
    pub xmlXPathCastBooleanToNumber: unsafe extern "C" fn(val: c_int) -> f64,
    pub xmlXPathCastNumberToString: unsafe extern "C" fn(val: f64) -> *mut c_uchar,
    pub xmlSetExternalEntityLoader: unsafe extern "C" fn(f: Option<xmlExternalEntityLoader>),
    pub xmlSetStructuredErrorFunc:
        unsafe extern "C" fn(ctx: *mut c_void, handler: Option<xmlStructuredErrorFunc>),
}

impl LibXml2 {
    // xmlFree is a GLOBAL VARIABLE holding a fn pointer (xmlmemory.h);
    // calling the variable's address as code is a SIGBUS — read per call.
    #[inline]
    pub unsafe fn xmlFree(&self, p: *mut c_void) {
        unsafe { (*self.xmlFreeVar)(p) }
    }
}

// SAFETY: one backend = one thread; the table is immutable fn pointers plus
// the xmlFree variable address, valid process-wide once resolved.
unsafe impl Sync for LibXml2 {}
unsafe impl Send for LibXml2 {}

static LIBXML: OnceLock<Result<&'static LibXml2, String>> = OnceLock::new();

/// The libxml2 table. Unloadable: raises the `unavailable_error` ERROR
/// through the panic path (`PgError` payload, `pg_error_from_panic`), the
/// shape C gives for a build without libxml.
pub fn xml2() -> &'static LibXml2 {
    match LIBXML.get_or_init(load) {
        Ok(api) => api,
        Err(e) => std::panic::panic_any(Box::new(unavailable_error(e))),
    }
}

/// What a loaded libxml2 looks like to `pgrust_runtime_status()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loaded {
    pub library: &'static str,
    pub version: Option<&'static str>,
}

/// Non-panicking probe for the control plane: Ok names the library that
/// loaded, Err is the probe failure (`probe_failure_message` or a missing
/// symbol). Resolves once and caches like `xml2()`.
pub fn status() -> Result<Loaded, &'static str> {
    match LIBXML.get_or_init(load) {
        Ok(api) => Ok(Loaded { library: api.library, version: api.version.as_deref() }),
        Err(e) => Err(e.as_str()),
    }
}

/// C's `NO_XML_SUPPORT()` ERROR for a host without a loadable libxml2:
/// same message, SQLSTATE and DETAIL as a PostgreSQL built without
/// `--with-libxml`, plus a HINT saying what the host is missing. The
/// previous shape was a bare backend panic ("could not dlopen libxml2.so.2")
/// reported as an internal error.
pub fn unavailable_error(detail: &str) -> PgError {
    PgError::error("unsupported XML feature")
        .with_sqlstate(ERRCODE_FEATURE_NOT_SUPPORTED)
        .with_detail("This functionality requires the server to be built with libxml support.")
        .with_hint(format!(
            "PGBoxDB binds libxml2 at run time and {detail}. Install the libxml2 shared library \
             on this host (Debian/Ubuntu package libxml2, named libxml2-16 from Ubuntu 25.10)."
        ))
}

/// dlopen candidates, in probe order. libxml2 2.14 changed the SONAME from
/// `libxml2.so.2` to `libxml2.so.16`; Ubuntu 25.10+ (26.04 LTS) ship only
/// `.so.16`, Debian 13 and older Ubuntu only `.so.2`, and the bare
/// `libxml2.so` symlink comes from the -dev package, which a runtime host
/// does not install. Probing `.so.2` alone was the "could not dlopen
/// libxml2.so.2" failure of every XML function on a PGBoxDB host.
#[cfg(all(not(target_family = "wasm"), not(target_os = "macos")))]
pub const CANDIDATES: &[&CStr] = &[c"libxml2.so.16", c"libxml2.so.2", c"libxml2.so"];

// macOS: the system libxml2 (dyld shared cache) is 2.9 = `libxml2.2.dylib`;
// a Homebrew libxml2 >= 2.14 is `libxml2.16.dylib`, keg-only, so its lib
// directory is probed by path (arm64, then x86).
#[cfg(target_os = "macos")]
pub const CANDIDATES: &[&CStr] = &[
    c"libxml2.2.dylib",
    c"libxml2.16.dylib",
    c"libxml2.dylib",
    c"/opt/homebrew/opt/libxml2/lib/libxml2.dylib",
    c"/usr/local/opt/libxml2/lib/libxml2.dylib",
];

#[cfg(target_family = "wasm")]
pub const CANDIDATES: &[&CStr] = &[];

pub fn probe_failure_message() -> String {
    let names: Vec<&str> = CANDIDATES.iter().map(|c| c.to_str().unwrap_or("?")).collect();
    format!("could not dlopen {}", names.join(" / "))
}

#[cfg(not(target_family = "wasm"))]
fn open_lib() -> Result<(*mut c_void, &'static str), String> {
    for name in CANDIDATES {
        // SAFETY: CStr literal is NUL-terminated.
        let h = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW) };
        if !h.is_null() {
            return Ok((h, name.to_str().unwrap_or("?")));
        }
    }
    Err(probe_failure_message())
}

// wasm32: no dynamic loading on wasm32-wasip1 (wasi-libc ships no dlopen),
// so libxml2 can never be resolved; every xml entry point that touches the
// table panics with this message — same shape as the native no-.so
// environment. A functional wasm xml arm needs a statically linked libxml2
// (later increment; xml is not on the P5 --single serial subset).
#[cfg(target_family = "wasm")]
fn load() -> Result<&'static LibXml2, String> {
    Err("xml is not supported on wasm32-wasip1 (no dynamic loading for libxml2)".to_string())
}

#[cfg(not(target_family = "wasm"))]
fn load() -> Result<&'static LibXml2, String> {
    let (handle, library) = open_lib()?;
    // xmlParserVersion is a `const char *` data symbol (parser.h): the
    // library's own version as "MMmmpp" digits. Optional — never a failure.
    let version = {
        // SAFETY: NUL-terminated name; handle is a live dlopen handle.
        let p = unsafe { libc::dlsym(handle, c"xmlParserVersion".as_ptr()) } as *const *const c_char;
        // SAFETY: a non-null xmlParserVersion points at a NUL-terminated
        // static string owned by libxml2 for the life of the process.
        unsafe {
            (!p.is_null() && !(*p).is_null())
                .then(|| CStr::from_ptr(*p).to_string_lossy().into_owned())
        }
    };
    macro_rules! resolve {
        ($name:ident) => {{
            let sym = concat!(stringify!($name), "\0");
            // SAFETY: sym is NUL-terminated; handle is a live dlopen handle.
            let p = unsafe { libc::dlsym(handle, sym.as_ptr() as *const c_char) };
            if p.is_null() {
                return Err(format!("libxml2 symbol {} not found", stringify!($name)));
            }
            // SAFETY: non-null dlsym result transmuted to the C signature
            // declared for this stable public libxml2 entry point.
            unsafe { core::mem::transmute(p) }
        }};
    }
    let xmlFreeVar = {
        // SAFETY: as above; xmlFree is a data symbol (see LibXml2::xmlFree).
        let p = unsafe { libc::dlsym(handle, c"xmlFree".as_ptr()) };
        if p.is_null() {
            return Err("libxml2 symbol xmlFree not found".to_string());
        }
        p as *const xmlFreeFunc
    };
    Ok(Box::leak(Box::new(LibXml2 {
        library,
        version,
        xmlInitParser: resolve!(xmlInitParser),
        xmlFreeVar,
        xmlStrdup: resolve!(xmlStrdup),
        xmlStrlen: resolve!(xmlStrlen),
        xmlNewParserCtxt: resolve!(xmlNewParserCtxt),
        xmlFreeParserCtxt: resolve!(xmlFreeParserCtxt),
        xmlCtxtReadDoc: resolve!(xmlCtxtReadDoc),
        xmlCtxtReadMemory: resolve!(xmlCtxtReadMemory),
        xmlNewDoc: resolve!(xmlNewDoc),
        xmlFreeDoc: resolve!(xmlFreeDoc),
        xmlParseBalancedChunkMemory: resolve!(xmlParseBalancedChunkMemory),
        xmlKeepBlanksDefault: resolve!(xmlKeepBlanksDefault),
        xmlDocSetRootElement: resolve!(xmlDocSetRootElement),
        xmlNewNode: resolve!(xmlNewNode),
        xmlNewDocText: resolve!(xmlNewDocText),
        xmlAddChildList: resolve!(xmlAddChildList),
        xmlFreeNode: resolve!(xmlFreeNode),
        xmlBufferCreate: resolve!(xmlBufferCreate),
        xmlBufferFree: resolve!(xmlBufferFree),
        xmlBufferContent: resolve!(xmlBufferContent),
        xmlBufferLength: resolve!(xmlBufferLength),
        xmlSaveToBuffer: resolve!(xmlSaveToBuffer),
        xmlSaveDoc: resolve!(xmlSaveDoc),
        xmlSaveTree: resolve!(xmlSaveTree),
        xmlSaveClose: resolve!(xmlSaveClose),
        xmlNodeDump: resolve!(xmlNodeDump),
        xmlCopyNode: resolve!(xmlCopyNode),
        xmlNewTextWriterMemory: resolve!(xmlNewTextWriterMemory),
        xmlFreeTextWriter: resolve!(xmlFreeTextWriter),
        xmlTextWriterStartElement: resolve!(xmlTextWriterStartElement),
        xmlTextWriterEndElement: resolve!(xmlTextWriterEndElement),
        xmlTextWriterWriteAttribute: resolve!(xmlTextWriterWriteAttribute),
        xmlTextWriterWriteRaw: resolve!(xmlTextWriterWriteRaw),
        xmlTextWriterWriteBase64: resolve!(xmlTextWriterWriteBase64),
        xmlTextWriterWriteBinHex: resolve!(xmlTextWriterWriteBinHex),
        xmlEncodeSpecialChars: resolve!(xmlEncodeSpecialChars),
        xmlXPathNewContext: resolve!(xmlXPathNewContext),
        xmlXPathFreeContext: resolve!(xmlXPathFreeContext),
        xmlXPathRegisterNs: resolve!(xmlXPathRegisterNs),
        xmlXPathCtxtCompile: resolve!(xmlXPathCtxtCompile),
        xmlXPathFreeCompExpr: resolve!(xmlXPathFreeCompExpr),
        xmlXPathCompiledEval: resolve!(xmlXPathCompiledEval),
        xmlXPathFreeObject: resolve!(xmlXPathFreeObject),
        xmlXPathCastNodeToString: resolve!(xmlXPathCastNodeToString),
        xmlXPathCastNodeSetToString: resolve!(xmlXPathCastNodeSetToString),
        xmlXPathCastBooleanToString: resolve!(xmlXPathCastBooleanToString),
        xmlXPathCastBooleanToNumber: resolve!(xmlXPathCastBooleanToNumber),
        xmlXPathCastNumberToString: resolve!(xmlXPathCastNumberToString),
        xmlSetExternalEntityLoader: resolve!(xmlSetExternalEntityLoader),
        xmlSetStructuredErrorFunc: resolve!(xmlSetStructuredErrorFunc),
    })))
}

#[inline]
pub unsafe fn node_type(node: *mut xmlNode) -> c_int {
    unsafe { (*(node as *const xmlNodeHdr)).type_ }
}

pub fn cstr(bytes: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(bytes.len() + 1);
    v.extend_from_slice(bytes);
    v.push(0);
    v
}

pub unsafe fn xmlchar_to_vec(p: *const c_uchar) -> Vec<u8> {
    if p.is_null() {
        return Vec::new();
    }
    // SAFETY: p is a NUL-terminated libxml string per caller contract.
    unsafe {
        let len = (xml2().xmlStrlen)(p) as usize;
        std::slice::from_raw_parts(p, len).to_vec()
    }
}

pub unsafe fn buffer_to_vec(buf: *mut xmlBuffer) -> Vec<u8> {
    // SAFETY: buf is a live xmlBuffer per caller contract.
    unsafe {
        let content = (xml2().xmlBufferContent)(buf);
        let len = (xml2().xmlBufferLength)(buf) as usize;
        if content.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(content, len).to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // libxml2 2.14 changed the SONAME from .so.2 to .so.16; Ubuntu 25.10+
    // ships only libxml2.so.16 and the bare libxml2.so belongs to the -dev
    // package. A runtime host must resolve with neither .so.2 nor -dev.
    #[cfg(target_os = "linux")]
    #[test]
    fn probes_new_soname_before_old_and_bare() {
        let names: Vec<&str> = CANDIDATES.iter().map(|c| c.to_str().unwrap()).collect();
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap_or_else(|| panic!("{n} not probed"));
        assert!(pos("libxml2.so.16") < pos("libxml2.so.2"));
        assert!(pos("libxml2.so.2") < pos("libxml2.so"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn probes_system_then_new_dylib_then_homebrew() {
        let names: Vec<&str> = CANDIDATES.iter().map(|c| c.to_str().unwrap()).collect();
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap_or_else(|| panic!("{n} not probed"));
        assert!(pos("libxml2.2.dylib") < pos("libxml2.16.dylib"));
        assert!(names.iter().any(|n| n.starts_with("/opt/homebrew/")));
    }

    // A missing library is C's "built without libxml" (0A000 + the same
    // DETAIL), never an internal error; the HINT names the package to install.
    #[test]
    fn unavailable_is_feature_not_supported_with_install_hint() {
        let e = unavailable_error("could not dlopen libxml2.so.16");
        assert_eq!(e.sqlstate(), ::types_error::ERRCODE_FEATURE_NOT_SUPPORTED);
        assert_eq!(e.message(), "unsupported XML feature");
        assert_eq!(
            e.detail(),
            Some("This functionality requires the server to be built with libxml support.")
        );
        let hint = e.hint().expect("hint");
        assert!(hint.contains("could not dlopen libxml2.so.16"), "{hint}");
        assert!(hint.contains("libxml2"), "{hint}");
    }

    // The error message lists every probed name so an operator can see what
    // the host lacks.
    #[test]
    fn probe_failure_lists_every_candidate() {
        let msg = probe_failure_message();
        for c in CANDIDATES {
            assert!(msg.contains(c.to_str().unwrap()), "{msg}");
        }
    }

    // On a host with libxml2 the status is Ok and names the library that
    // loaded; without it, Err carries the probe message. Either way the probe
    // never panics — pgrust_runtime_status() reports it.
    #[test]
    fn status_never_panics() {
        match status() {
            Ok(loaded) => assert!(CANDIDATES.iter().any(|c| c.to_str().unwrap() == loaded.library)),
            Err(e) => assert!(e.contains("could not dlopen"), "{e}"),
        }
    }
}
