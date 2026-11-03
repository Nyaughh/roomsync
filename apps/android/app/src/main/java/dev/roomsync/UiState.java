package dev.roomsync;

/** Plain-language presentation only. Never changes the receiver or infers acoustic sync. */
final class UiState {
    final String badge,title,detail;
    final boolean busy,warning;
    private UiState(String badge,String title,String detail,boolean busy,boolean warning){this.badge=badge;this.title=title;this.detail=detail;this.busy=busy;this.warning=warning;}
    static UiState from(String raw,boolean running,boolean paired){
        String s=raw==null?"":raw;
        if(running){
            if(s.startsWith("Stopping"))return new UiState("Disconnecting","Finishing up","Closing the connection safely.",true,false);
            if(s.contains("different pairing version"))return new UiState("Update needed","Update both apps","Windows and this phone are using different connection versions.",false,true);
            if(s.contains("authentication failed"))return new UiState("Setup not verified","Check the setup code","Disconnect, then scan a fresh QR code from Windows.",false,true);
            if(s.contains("unexpected address or port"))return new UiState("Check setup","Connection details don’t match","Disconnect and scan the current Windows QR code.",false,true);
            if(s.contains("peer unavailable"))return new UiState("Retrying","Can’t reach Windows yet","Check that RoomSync is open and both devices use the same Wi-Fi.",true,true);
            if(s.contains("Audio focus: temporarily muted"))return new UiState("Connected","Temporarily muted","Waiting for another app to release audio focus.",false,false);
            if(s.contains("No playable samples for this output deadline"))return new UiState("Connected","Audio interrupted","Some audio missed its playback time. Open connection details if this continues.",false,true);
            if(s.contains("Nonzero audio rendered"))return new UiState("Connected","Receiving audio",s.contains("Audio focus: temporarily lowered")?"Volume is lowered while another app uses audio.":"Adjust the volume with your phone’s media controls.",false,false);
            if(s.contains("Received audio for this deadline is silent"))return new UiState("Connected","Ready for audio","Play something on Windows to hear it here.",false,false);
            if(s.startsWith("Calibrating"))return new UiState("Preparing","Setting up audio","Measuring this phone’s output. This takes a moment.",true,false);
            if(s.startsWith("Output calibrated"))return new UiState("Waiting","Waiting for Windows","Keep RoomSync open on your computer. Other phones may still be connecting.",true,false);
            if(s.contains("Waiting for the synchronized start")||s.startsWith("Waiting for host clock")||s.startsWith("Rendering against"))return new UiState("Preparing","Starting together","Waiting for the shared audio start.",true,false);
            return new UiState("Connecting","Connecting to Windows","Keep RoomSync open on your computer and use the same Wi-Fi.",true,false);
        }
        if(s.startsWith("Stopped:")||s.startsWith("Unable to start")||s.startsWith("Another app")||s.startsWith("Audio ")||s.startsWith("Output route changed")){
            String reason=s.split("\\n",2)[0];while(reason.startsWith("Stopped: "))reason=reason.substring(9);
            if(reason.contains("Host ended"))reason="Windows ended the session. Start it again, then reconnect.";
            if(reason.length()>200)reason=reason.substring(0,197)+"…";
            return new UiState("Disconnected","Connection stopped",reason,false,true);
        }
        return paired?new UiState("Computer added","Ready to connect","Your saved setup is ready for this app session.",false,false):new UiState("Not connected","Connect your computer","Open RoomSync on Windows, then scan its QR code.",false,false);
    }
}
