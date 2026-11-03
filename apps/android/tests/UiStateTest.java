package dev.roomsync;
public final class UiStateTest {
    static void check(boolean b){if(!b)throw new AssertionError();}
    public static void main(String[] args){
        check(UiState.from("Ready",false,false).title.equals("Connect your computer"));
        check(UiState.from("Ready",false,true).title.equals("Ready to connect"));
        check(UiState.from("Starting encrypted receiver…",true,true).busy);
        check(UiState.from("Calibrating measured audio output…",true,true).title.equals("Setting up audio"));
        check(UiState.from("Output calibrated. Waiting for the shared playback timeline…",true,true).title.equals("Waiting for Windows"));
        check(!UiState.from("Rendering against measured audio timestamps\nEncrypted session ready. Waiting for the synchronized start or fresh clock lock.",true,true).title.equals("Receiving audio"));
        check(UiState.from("Rendering against measured audio timestamps\nNonzero audio rendered against measured timestamps.",true,true).title.equals("Receiving audio"));
        check(UiState.from("Received audio for this deadline is silent.",true,true).title.equals("Ready for audio"));
        check(UiState.from("No playable samples for this output deadline.",true,true).warning);
        check(UiState.from("Nonzero audio rendered\nAudio focus: temporarily muted",true,true).title.equals("Temporarily muted"));
        check(UiState.from("Stopped: Stopped: Host ended the session\nHidden metrics",false,true).detail.equals("Windows ended the session. Start it again, then reconnect."));
        check(UiState.from("Stopped by you. Tap Connect to reconnect.",false,true).title.equals("Ready to connect"));
        check(UiState.from("Output route changed. Reconnect to recalibrate.",false,true).warning);
        check(UiState.from("Peer reached us but uses a different pairing version; update both apps",true,true).title.equals("Update both apps"));
        check(UiState.from("UDP reached us but authentication failed; verify the setup code",true,true).warning);
        check(UiState.from("The operating system reported the peer unavailable; still retrying",true,true).busy);
        check(!UiState.from("",false,false).busy);
        System.out.println("UiStateTest: empty, ready, connecting, calibration, waiting, PCM/silence/missing, focus, stop and retry states passed");
    }
}
