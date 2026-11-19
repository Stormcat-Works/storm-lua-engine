//! アイドル境界でVMヒープの外にある実行状態へアクセスするcontinuation API。
//!
//! `Vm::snapshot` はヒープの到達可能グラフだけを運びます。乱数ストリームのように
//! ヒープの外で状態を持つものは、continuation 側がこのモジュールの API で明示的に
//! 往復させます。復元順序は「ヒープ → ヒープ外」で、復元後は同じ列が続きます。
use crate::runner::{ErrorKind, RandomStreamState, Vm, VmError};
use std::sync::atomic::Ordering;

impl Vm {
    /// アイドル境界で `math.random` ストリームの状態を返します。
    ///
    /// 既定の車両VMは [`RandomStreamState::Engine`] を返し、値は次回生成前の
    /// ベース状態です（`math.randomseed` が書き込むのと同じスロット）。
    /// [`Vm::install_crt_random`] 済みのVMはホストがストリームを所有するため
    /// [`RandomStreamState::HostOwned`] を返し、車両用ストリームとして読ませません。
    pub fn random_stream_state(&self) -> Result<RandomStreamState, VmError> {
        self.ensure_idle()?;
        match &self.random_stream {
            crate::sandbox::MathRandom::Local(stream) => {
                Ok(RandomStreamState::Engine(stream.load(Ordering::Relaxed)))
            }
            crate::sandbox::MathRandom::BuiltinCrt(crt) => {
                Ok(RandomStreamState::BuiltinCrt(crt.state()))
            }
            crate::sandbox::MathRandom::HostShared => Ok(RandomStreamState::HostOwned),
        }
    }

    /// アイドル境界でエンジン所有の `math.random` ストリーム状態を置き換えます。
    ///
    /// ホスト共有CRTストリームのVMでは状態がホストのものなので、黙って無視せず
    /// `Unsupported` で拒否します。既存の生成規則・初期値は変えません。
    pub fn set_random_stream_state(&mut self, state: RandomStreamState) -> Result<(), VmError> {
        self.ensure_idle()?;
        match (&self.random_stream, state) {
            (crate::sandbox::MathRandom::Local(stream), RandomStreamState::Engine(value)) => {
                stream.store(value, Ordering::Relaxed);
                Ok(())
            }
            (crate::sandbox::MathRandom::Local(_), RandomStreamState::HostOwned) => {
                Err(VmError::new(
                    ErrorKind::InvalidArgument,
                    "this VM owns an engine random stream; HostOwned has no state to apply",
                ))
            }
            (crate::sandbox::MathRandom::BuiltinCrt(crt), RandomStreamState::BuiltinCrt(value)) => {
                crt.set_state(value);
                Ok(())
            }
            (crate::sandbox::MathRandom::Local(_), RandomStreamState::BuiltinCrt(_))
            | (crate::sandbox::MathRandom::BuiltinCrt(_), RandomStreamState::Engine(_))
            | (crate::sandbox::MathRandom::BuiltinCrt(_), RandomStreamState::HostOwned) => {
                Err(VmError::new(
                    ErrorKind::InvalidArgument,
                    "random stream state kind does not match this VM's stream",
                ))
            }
            (crate::sandbox::MathRandom::HostShared, _) => Err(VmError::new(
                ErrorKind::Unsupported,
                "random stream is owned by the host CRT provider",
            )),
        }
    }
}
