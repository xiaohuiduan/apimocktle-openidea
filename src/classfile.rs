//! A minimal Java class-file reader: just enough of the format to describe a
//! DTO that lives in a dependency jar.
//!
//! The interesting parts are the field table and the generic `Signature`
//! attributes. Without them a wrapper such as `ApiEnvelope<T>` would only
//! show `Object result`, and `ApiEnvelope<Map<String, List<Item>>>` could not
//! substitute `Item` into the jar class's `result` field.

/// A field as declared in the class file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JarField {
    pub name: String,
    pub ty: String,
}

/// What the plugin needs to know about one class.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassFile {
    /// Simple name (`ApiEnvelope`, `Outer$Inner`).
    pub name: String,
    /// Simple name of the superclass; `None` for `java.lang.Object`.
    pub super_class: Option<String>,
    /// Declared type parameters, in order (`class ApiEnvelope<T>` → `["T"]`).
    pub params: Vec<String>,
    /// Own fields only; inherited ones are merged by the resolver.
    pub fields: Vec<JarField>,
    /// Type arguments of the superclass (`extends Base<UserDto>` → `["UserDto"]`).
    pub super_args: Vec<String>,
}

const ACC_STATIC: u16 = 0x0008;
const ACC_SYNTHETIC: u16 = 0x1000;

/// Parses one `.class` file. `None` means the bytes are not a class file this
/// reader understands (a newer format, a module descriptor, garbage).
pub fn parse(bytes: &[u8]) -> Option<ClassFile> {
    let mut r = Reader { data: bytes, at: 0 };
    if r.u32()? != 0xCAFE_BABE {
        return None;
    }
    let _minor = r.u16()?;
    let _major = r.u16()?;

    let count = r.u16()? as usize;
    let mut cp = vec![Cp::Other; count.max(1)];
    let mut i = 1;
    while i < count {
        match r.u8()? {
            1 => {
                let len = r.u16()? as usize;
                cp[i] = Cp::Utf8(String::from_utf8_lossy(r.take(len)?).into_owned());
            }
            7 => cp[i] = Cp::Class(r.u16()?),
            8 | 16 | 19 | 20 => r.skip(2)?,
            15 => r.skip(3)?,
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => r.skip(4)?,
            5 | 6 => {
                r.skip(8)?;
                i += 1; // long and double take two constant-pool slots
            }
            _ => return None,
        }
        i += 1;
    }

    let _access = r.u16()?;
    let this_class = r.u16()?;
    let super_index = r.u16()?;
    let interfaces = r.u16()? as usize;
    r.skip(interfaces.checked_mul(2)?)?;

    let this_name = simple(&cp.class_name(this_class)?);
    let mut class = ClassFile {
        name: this_name,
        super_class: match super_index {
            0 => None,
            index => Some(simple(&cp.class_name(index)?)),
        },
        ..ClassFile::default()
    };

    let field_count = r.u16()? as usize;
    for _ in 0..field_count {
        let flags = r.u16()?;
        let name = cp.utf8(r.u16()?)?;
        let descriptor = cp.utf8(r.u16()?)?;
        let mut signature = None;
        let attributes = r.u16()? as usize;
        for _ in 0..attributes {
            let attr = cp.utf8(r.u16()?)?;
            let len = r.u32()? as usize;
            let body = r.take(len)?;
            // A Signature attribute's body is the index of the signature string.
            if attr == "Signature"
                && len == 2
            {
                signature = cp.utf8(u16::from_be_bytes([body[0], body[1]]));
            }
        }
        if flags & (ACC_STATIC | ACC_SYNTHETIC) != 0 || name == "serialVersionUID" || name.starts_with("this$") {
            continue;
        }
        let ty = signature
            .as_deref()
            .and_then(field_type)
            .or_else(|| descriptor_type(&descriptor))
            .unwrap_or_else(|| "Object".to_owned());
        class.fields.push(JarField { name, ty });
    }

    let method_count = r.u16()? as usize;
    for _ in 0..method_count {
        r.skip(6)?; // access flags, name, descriptor
        let attributes = r.u16()? as usize;
        for _ in 0..attributes {
            r.skip(2)?;
            let len = r.u32()? as usize;
            r.skip(len)?;
        }
    }

    let attributes = r.u16()? as usize;
    for _ in 0..attributes {
        let attr = cp.utf8(r.u16()?)?;
        let len = r.u32()? as usize;
        let body = r.take(len)?;
        // A Signature attribute's body is the index of the signature string.
        if attr == "Signature"
            && len == 2
            && let Some(sig) = cp.utf8(u16::from_be_bytes([body[0], body[1]]))
            && let Some((params, super_ty)) = class_signature(&sig)
        {
            class.params = params;
            if let Ty::Class(super_name, super_args) = super_ty {
                class.super_class = Some(simple(&super_name));
                class.super_args = super_args.iter().map(Ty::render).collect();
            }
        }
    }
    Some(class)
}

/// The one type a field signature names (`Ljava/util/List<TT;>;` → `List<T>`).
fn field_type(sig: &str) -> Option<String> {
    let bytes = sig.as_bytes();
    let mut at = 0;
    let ty = parse_ty(bytes, &mut at)?;
    (at == bytes.len()).then(|| ty.render())
}

/// A field descriptor without generic information (`Ljava/lang/Long;` → `Long`).
fn descriptor_type(descriptor: &str) -> Option<String> {
    let bytes = descriptor.as_bytes();
    let mut at = 0;
    let ty = parse_ty(bytes, &mut at)?;
    (at == bytes.len()).then(|| ty.render())
}

/// The declared type parameters and the superclass of a class signature.
fn class_signature(sig: &str) -> Option<(Vec<String>, Ty)> {
    let bytes = sig.as_bytes();
    let mut at = 0;
    let mut params = Vec::new();
    if bytes.first() == Some(&b'<') {
        at = 1;
        while peek(bytes, at)? != b'>' {
            let start = at;
            while peek(bytes, at)? != b':' {
                at += 1;
            }
            params.push(std::str::from_utf8(&bytes[start..at]).ok()?.to_owned());
            at += 1; // ':'
            if matches!(peek(bytes, at), Some(b'L' | b'T' | b'[')) {
                parse_ty(bytes, &mut at)?; // the class bound
            }
            while peek(bytes, at) == Some(b':') {
                at += 1;
                parse_ty(bytes, &mut at)?; // interface bounds
            }
        }
        at += 1; // '>'
    }
    let super_ty = parse_ty(bytes, &mut at)?;
    (at == bytes.len()).then_some((params, super_ty))
}

#[derive(Clone, Debug)]
enum Ty {
    Prim(&'static str),
    Var(String),
    Class(String, Vec<Ty>),
    Array(Box<Ty>, usize),
}

impl Ty {
    fn render(&self) -> String {
        match self {
            Ty::Prim(name) => (*name).to_owned(),
            Ty::Var(name) => name.clone(),
            Ty::Class(name, args) => {
                let mut out = simple(name);
                if !args.is_empty() {
                    out.push('<');
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&arg.render());
                    }
                    out.push('>');
                }
                out
            }
            Ty::Array(inner, depth) => {
                let mut out = inner.render();
                for _ in 0..*depth {
                    out.push_str("[]");
                }
                out
            }
        }
    }
}

fn peek(bytes: &[u8], at: usize) -> Option<u8> {
    bytes.get(at).copied()
}

fn parse_ty(bytes: &[u8], at: &mut usize) -> Option<Ty> {
    match peek(bytes, *at)? {
        b'[' => {
            let mut depth = 0;
            while peek(bytes, *at) == Some(b'[') {
                depth += 1;
                *at += 1;
            }
            Some(Ty::Array(Box::new(parse_ty(bytes, at)?), depth))
        }
        b'T' => {
            *at += 1;
            let start = *at;
            while peek(bytes, *at)? != b';' {
                *at += 1;
            }
            let name = std::str::from_utf8(&bytes[start..*at]).ok()?.to_owned();
            *at += 1;
            Some(Ty::Var(name))
        }
        b'L' => {
            *at += 1;
            let start = *at;
            while !matches!(peek(bytes, *at), Some(b'<' | b';')) {
                *at += 1;
            }
            let name = std::str::from_utf8(&bytes[start..*at]).ok()?.to_owned();
            let mut args = Vec::new();
            if peek(bytes, *at) == Some(b'<') {
                *at += 1;
                while peek(bytes, *at)? != b'>' {
                    args.push(parse_ty(bytes, at)?);
                }
                *at += 1;
            }
            if peek(bytes, *at)? != b';' {
                return None;
            }
            *at += 1;
            Some(Ty::Class(name, args))
        }
        b'*' => {
            *at += 1;
            Some(Ty::Class("java/lang/Object".to_owned(), Vec::new()))
        }
        b'+' | b'-' => {
            *at += 1;
            parse_ty(bytes, at)
        }
        b'B' => primitive(at, "byte"),
        b'C' => primitive(at, "char"),
        b'D' => primitive(at, "double"),
        b'F' => primitive(at, "float"),
        b'I' => primitive(at, "int"),
        b'J' => primitive(at, "long"),
        b'S' => primitive(at, "short"),
        b'Z' => primitive(at, "boolean"),
        b'V' => primitive(at, "void"),
        _ => None,
    }
}

fn primitive(at: &mut usize, name: &'static str) -> Option<Ty> {
    *at += 1;
    Some(Ty::Prim(name))
}

fn simple(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_owned()
}

#[derive(Clone, Debug)]
enum Cp {
    Utf8(String),
    Class(u16),
    Other,
}

/// Reads the constant pool through the two typed accessors the parser needs.
trait ConstantPool {
    fn utf8(&self, index: u16) -> Option<String>;
    fn class_name(&self, index: u16) -> Option<String>;
}

impl ConstantPool for Vec<Cp> {
    fn utf8(&self, index: u16) -> Option<String> {
        match self.get(index as usize)? {
            Cp::Utf8(text) => Some(text.clone()),
            _ => None,
        }
    }

    fn class_name(&self, index: u16) -> Option<String> {
        match self.get(index as usize)? {
            Cp::Class(name) => self.utf8(*name),
            _ => None,
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let slice = self.data.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn skip(&mut self, len: usize) -> Option<()> {
        self.take(len).map(|_| ())
    }

    fn u8(&mut self) -> Option<u8> {
        Some(*self.take(1)?.first()?)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::Zip;

    fn fixture() -> ClassFile {
        let jar = include_bytes!("../fixtures/deps/api-envelope.jar");
        let zip = Zip::open(jar).expect("the fixture is a jar");
        let entry = zip.find("/ApiEnvelope.class").expect("the class entry");
        let bytes = zip.read(entry, 1 << 20).expect("the class bytes");
        parse(&bytes).expect("a class file")
    }

    #[test]
    fn reads_fields_and_type_parameters() {
        let class = fixture();
        assert_eq!(class.name, "ApiEnvelope");
        assert_eq!(class.params, vec!["T".to_owned()]);
        assert_eq!(class.super_class.as_deref(), Some("BaseReply"));
        assert_eq!(
            class.fields,
            vec![
                JarField { name: "errorNo".into(), ty: "Long".into() },
                JarField { name: "errorInfo".into(), ty: "String".into() },
                JarField { name: "result".into(), ty: "T".into() },
            ],
            "the generic field keeps T: {class:?}"
        );
    }

    #[test]
    fn renders_generic_signatures() {
        assert_eq!(field_type("TT;").as_deref(), Some("T"));
        assert_eq!(field_type("Ljava/util/List<TT;>;").as_deref(), Some("List<T>"));
        assert_eq!(
            field_type("Ljava/util/Map<Ljava/lang/String;Ljava/util/List<Lcom/acme/Item;>;>;").as_deref(),
            Some("Map<String, List<Item>>")
        );
        assert_eq!(field_type("Ljava/util/List<+Lcom/acme/Base;>;").as_deref(), Some("List<Base>"));
        assert_eq!(descriptor_type("[[I").as_deref(), Some("int[][]"));
        assert_eq!(descriptor_type("Lcom/acme/User$Inner;").as_deref(), Some("User$Inner"));
    }

    #[test]
    fn reads_a_generic_superclass() {
        let (params, super_ty) = class_signature("<T:Ljava/lang/Object;>Lcom/acme/Base<TT;>;").expect("a signature");
        assert_eq!(params, vec!["T".to_owned()]);
        assert_eq!(super_ty.render(), "Base<T>");
        let (params, super_ty) =
            class_signature("<T::Ljava/lang/Comparable<TT;>;>Ljava/lang/Object;").expect("bounds");
        assert_eq!(params, vec!["T".to_owned()]);
        assert_eq!(super_ty.render(), "Object");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"not a class").is_none());
        assert!(parse(&[]).is_none());
    }
}
