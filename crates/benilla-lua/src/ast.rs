//! Byte strings the compiler, the heap and hot reload share: names, sources, function keys.

use std::rc::Rc;

pub type Bytes = Rc<[u8]>;
