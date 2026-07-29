use galaxy_terminal::model::escape_sequences;

use super::AIAgentPtyWriteMode;

#[test]
fn raw_etx_is_the_only_shell_interrupt_payload() {
    assert!(AIAgentPtyWriteMode::Raw.is_shell_interrupt(&[escape_sequences::C0::ETX]));
    assert!(!AIAgentPtyWriteMode::Raw.is_shell_interrupt(b"C-c"));
    assert!(!AIAgentPtyWriteMode::Raw.is_shell_interrupt(br"\u0003"));
    assert!(!AIAgentPtyWriteMode::Line.is_shell_interrupt(&[escape_sequences::C0::ETX]));
    assert!(!AIAgentPtyWriteMode::Block.is_shell_interrupt(&[escape_sequences::C0::ETX]));
}
