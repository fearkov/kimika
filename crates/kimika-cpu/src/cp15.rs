//! the parts of the system control coprocessor user code can reach: the
//! thread ID registers, one of which holds the thread's TLS pointer. the
//! barriers CP15 once had are left to the decoder, which runs them as the
//! barrier instructions they became.

#[derive(Debug, Clone, Default)]
pub struct Cp15 {
    /// c13,c0,2, TPIDRURW, the thread ID user code can write.
    pub thread_id_rw: u32,
    /// c13,c0,3, TPIDRURO, the one it can only read, which the Vita points
    /// at the end of the thread's 0x800 byte block of TLS.
    pub thread_id_ro: u32,
}

impl Cp15 {
    /// MRC p15, opc1, Rt, CRn, CRm, opc2, none for a register user mode
    /// can't read.
    pub fn read(&self, opc1: u32, crn: u32, crm: u32, opc2: u32) -> Option<u32> {
        match (opc1, crn, crm, opc2) {
            (0, 13, 0, 2) => Some(self.thread_id_rw),
            (0, 13, 0, 3) => Some(self.thread_id_ro),
            _ => None,
        }
    }

    /// MCR p15, opc1, Rt, CRn, CRm, opc2, false for a register user mode
    /// can't write.
    pub fn write(&mut self, opc1: u32, crn: u32, crm: u32, opc2: u32, value: u32) -> bool {
        match (opc1, crn, crm, opc2) {
            (0, 13, 0, 2) => self.thread_id_rw = value,
            _ => return false,
        }
        true
    }
}
