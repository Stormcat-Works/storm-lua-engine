//! プロパティは設定情報であり、Compositeのf32転送とは独立しています。
use std::collections::BTreeMap;

/// プロパティペイロード。数値の保持は、ゲーム精度に関する検証結果が出るまでf64を維持します。
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    /// Composite信号の量子化（f32丸め）を継承しません。
    Number(f64),
    /// 厳密なブール値。数値やテキストからの型強制は行いません。
    Bool(bool),
    /// NULや不正なUTF-8を含む、Lua互換のバイト列。
    Text(Vec<u8>),
}

/// 大文字小文字を区別するバイト列ラベルと型付きプロパティ値。
/// 永続化、UIラベル、スライダー、重複定義ポリシーはホスト側の責務です。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PropertyBag(BTreeMap<Vec<u8>, PropertyValue>);
impl PropertyBag {
    /// 明示的な更新。置換された値がある場合はそれを返します。
    pub fn insert(&mut self, label: Vec<u8>, value: PropertyValue) -> Option<PropertyValue> {
        self.0.insert(label, value)
    }
    /// テキストデコードや数値の型縮小を行わずに、保持されている値を参照します。
    pub fn get(&self, label: &[u8]) -> Option<&PropertyValue> {
        self.0.get(label)
    }
    /// 明示的な削除（空文字列の挿入とは区別されます）。
    pub fn remove(&mut self, label: &[u8]) -> Option<PropertyValue> {
        self.0.remove(label)
    }
    /// Borrow the complete typed entries without decoding labels or text bytes.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &PropertyValue)> {
        self.0
            .iter()
            .map(|(label, value)| (label.as_slice(), value))
    }
    /// プロパティ定義の数。
    pub fn len(&self) -> usize {
        self.0.len()
    }
    /// プロパティが空かどうか。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
