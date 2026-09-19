//! Just enough of the WebAssembly binary format to check a module against a sidecar at
//! compile time: the types, the imports, and the exports. The compiler stays
//! dependency-free, and a mismatched signature becomes a build error instead of a trap
//! somewhere inside a running program.

/// A WebAssembly value type. Anything richer than a number is `Other`: the boundary
/// carries numbers, and everything else is rejected with the type's own name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
    Other,
}

impl ValType {
    pub fn name(self) -> &'static str {
        match self {
            ValType::I32 => "i32",
            ValType::I64 => "i64",
            ValType::F32 => "f32",
            ValType::F64 => "f64",
            ValType::Other => "a non-numeric type",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

#[derive(Debug)]
pub struct Module {
    types: Vec<FuncType>,
    /// Type index per function, imported functions first, as the index space is ordered.
    functions: Vec<u32>,
    /// Exported name to function index.
    exports: Vec<(String, u32)>,
    pub exports_memory: bool,
    pub imports: Vec<(String, String)>,
}

impl Module {
    /// The signature of an exported function, or None when the module exports no
    /// function under that name.
    pub fn export(&self, name: &str) -> Option<&FuncType> {
        let index = self.exports.iter().find(|(export, _)| export == name)?.1;
        let type_index = *self.functions.get(index as usize)?;
        self.types.get(type_index as usize)
    }

    pub fn exported_function_names(&self) -> Vec<&str> {
        self.exports.iter().map(|(name, _)| name.as_str()).collect()
    }

    pub fn parse(bytes: &[u8]) -> Result<Module, String> {
        let mut reader = Reader { bytes, pos: 0 };
        let magic = reader.take(4)?;
        if magic != b"\0asm" {
            return Err("not a WebAssembly module (bad magic bytes)".into());
        }
        let version = reader.take(4)?;
        if version != [1, 0, 0, 0] {
            return Err(format!(
                "unsupported WebAssembly binary version {:?}; the compiler reads version 1",
                version
            ));
        }

        let mut module = Module {
            types: Vec::new(),
            functions: Vec::new(),
            exports: Vec::new(),
            exports_memory: false,
            imports: Vec::new(),
        };
        let mut defined: Vec<u32> = Vec::new();

        while reader.pos < bytes.len() {
            let id = reader.byte()?;
            let length = reader.leb128()? as usize;
            let end = reader.pos.checked_add(length).filter(|end| *end <= bytes.len())
                .ok_or("truncated WebAssembly section")?;
            match id {
                1 => module.types = reader.type_section()?,
                2 => {
                    for (module_name, name, function_type) in reader.import_section()? {
                        module.imports.push((module_name, name));
                        // Imported functions come first in the function index space.
                        if let Some(type_index) = function_type {
                            module.functions.push(type_index);
                        }
                    }
                }
                3 => defined = reader.vector(|r| r.leb128())?,
                7 => {
                    for (name, kind, index) in reader.export_section()? {
                        match kind {
                            0 => module.exports.push((name, index)),
                            2 if name == "memory" => module.exports_memory = true,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            // Sections the compiler does not read are skipped whole, and a section that
            // read short or long is still left at its declared end.
            reader.pos = end;
        }
        module.functions.extend(defined);
        Ok(module)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let byte = *self.bytes.get(self.pos).ok_or("truncated WebAssembly module")?;
        self.pos += 1;
        Ok(byte)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(count).filter(|end| *end <= self.bytes.len())
            .ok_or("truncated WebAssembly module")?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// An unsigned LEB128 integer, as every length and index in the format is encoded.
    fn leb128(&mut self) -> Result<u32, String> {
        let mut value: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            value |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 31 {
                return Err("oversized integer in WebAssembly module".into());
            }
        }
        u32::try_from(value).map_err(|_| "oversized integer in WebAssembly module".to_string())
    }

    fn name(&mut self) -> Result<String, String> {
        let length = self.leb128()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "non-UTF-8 name in WebAssembly module".into())
    }

    fn vector<T>(&mut self, mut item: impl FnMut(&mut Self) -> Result<T, String>) -> Result<Vec<T>, String> {
        let count = self.leb128()? as usize;
        let mut items = Vec::new();
        for _ in 0..count {
            items.push(item(self)?);
        }
        Ok(items)
    }

    fn val_type(&mut self) -> Result<ValType, String> {
        Ok(match self.byte()? {
            0x7f => ValType::I32,
            0x7e => ValType::I64,
            0x7d => ValType::F32,
            0x7c => ValType::F64,
            _ => ValType::Other,
        })
    }

    fn type_section(&mut self) -> Result<Vec<FuncType>, String> {
        self.vector(|r| {
            // Anything that is not a function type still occupies an index, so it is
            // recorded as an empty signature rather than dropped.
            if r.byte()? != 0x60 {
                return Ok(FuncType { params: Vec::new(), results: vec![ValType::Other] });
            }
            let params = r.vector(|r| r.val_type())?;
            let results = r.vector(|r| r.val_type())?;
            Ok(FuncType { params, results })
        })
    }

    fn import_section(&mut self) -> Result<Vec<(String, String, Option<u32>)>, String> {
        self.vector(|r| {
            let module_name = r.name()?;
            let name = r.name()?;
            let function_type = match r.byte()? {
                0x00 => Some(r.leb128()?),
                0x01 => { r.table_type()?; None }
                0x02 => { r.limits()?; None }
                0x03 => { r.val_type()?; r.byte()?; None }
                other => return Err(format!("unknown import kind {other} in WebAssembly module")),
            };
            Ok((module_name, name, function_type))
        })
    }

    fn table_type(&mut self) -> Result<(), String> {
        self.val_type()?;
        self.limits()
    }

    fn limits(&mut self) -> Result<(), String> {
        let flags = self.byte()?;
        self.leb128()?;
        if flags & 0x01 != 0 {
            self.leb128()?;
        }
        Ok(())
    }

    fn export_section(&mut self) -> Result<Vec<(String, u8, u32)>, String> {
        self.vector(|r| {
            let name = r.name()?;
            let kind = r.byte()?;
            let index = r.leb128()?;
            Ok((name, kind, index))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A section, with its length measured rather than counted by hand.
    fn section(id: u8, content: Vec<u8>) -> Vec<u8> {
        let mut bytes = vec![id, content.len() as u8];
        bytes.extend(content);
        bytes
    }

    fn name(text: &str) -> Vec<u8> {
        let mut bytes = vec![text.len() as u8];
        bytes.extend(text.as_bytes());
        bytes
    }

    /// The shape the sandbox codegen produces, compiled to wasm32-wasip1: one exported
    /// function, an exported memory, and a WASI import it never gets granted.
    fn module() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(b"\0asm");
        bytes.extend([1, 0, 0, 0]);
        // types: 0 = (i64) -> i64, 1 = () -> ()
        bytes.extend(section(1, vec![2, 0x60, 1, 0x7e, 1, 0x7e, 0x60, 0, 0]));
        // one imported function of type 1, which takes function index 0
        let mut imports = vec![1];
        imports.extend(name("wasi"));
        imports.extend(name("exit"));
        imports.extend([0x00, 1]);
        bytes.extend(section(2, imports));
        // one defined function of type 0, at function index 1
        bytes.extend(section(3, vec![1, 0]));
        let mut exports = vec![2];
        exports.extend(name("run"));
        exports.extend([0x00, 1]);
        exports.extend(name("memory"));
        exports.extend([0x02, 0]);
        bytes.extend(section(7, exports));
        bytes
    }

    #[test]
    fn reads_exports_past_imported_functions() {
        let parsed = Module::parse(&module()).unwrap();
        // The import occupies function index 0, so "run" must resolve to type 0, not 1.
        assert_eq!(
            parsed.export("run"),
            Some(&FuncType { params: vec![ValType::I64], results: vec![ValType::I64] })
        );
        assert!(parsed.exports_memory);
        assert_eq!(parsed.imports, vec![("wasi".to_string(), "exit".to_string())]);
        assert_eq!(parsed.exported_function_names(), vec!["run"]);
        assert!(parsed.export("missing").is_none());
    }

    #[test]
    fn rejects_files_that_are_not_modules() {
        assert!(Module::parse(b"not wasm at all").unwrap_err().contains("bad magic"));
        assert!(Module::parse(b"\0asm\x02\0\0\0").unwrap_err().contains("version"));
        assert!(Module::parse(b"\0asm").unwrap_err().contains("truncated"));
    }

    #[test]
    fn a_truncated_section_is_an_error_not_a_panic() {
        let mut bytes = module();
        bytes.truncate(bytes.len() - 4);
        assert!(Module::parse(&bytes).is_err());
    }
}
