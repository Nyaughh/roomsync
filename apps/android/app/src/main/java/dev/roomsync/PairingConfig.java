package dev.roomsync;

import java.util.Arrays;

/** Private, non-URL setup code. Parsing never resolves names or opens a network. */
final class PairingConfig {
    static final int MAX_CODE_LENGTH=256;
    final String host; final int hostPort,localPort,channel; final char[] key;
    PairingConfig(String host,int hostPort,int localPort,char[] key,int channel){
        validateHost(host);validatePort(hostPort);validatePort(localPort);
        if(channel<0||channel>3)throw new IllegalArgumentException("Choose a valid output channel");
        if(key==null||key.length!=64)throw new IllegalArgumentException("The pairing key must contain exactly 64 hex characters");
        for(char c:key)if(!((c>='0'&&c<='9')||(c>='a'&&c<='f')||(c>='A'&&c<='F')))throw new IllegalArgumentException("Pairing key must contain hexadecimal characters only");
        this.host=host;this.hostPort=hostPort;this.localPort=localPort;this.key=key.clone();this.channel=channel;
    }
    static PairingConfig parse(String input,int channel){
        if(input==null||input.length()>MAX_CODE_LENGTH)throw new IllegalArgumentException("Setup code is missing or too long. Copy a fresh code from RoomSync on Windows.");
        String code=input.trim();
        if(java.util.regex.Pattern.compile("\\s").matcher(code).find())throw new IllegalArgumentException("Setup code has a space or line break inside it. Copy the whole code again.");
        String[] parts=code.split("\\|",-1);
        if(parts.length!=5||!"RS1".equals(parts[0]))throw new IllegalArgumentException("This is not a supported RoomSync setup code. Update both apps and copy a new code.");
        char[] secret=parts[4].toCharArray();
        try{return new PairingConfig(parts[1],parsePort(parts[2]),parsePort(parts[3]),secret,channel);}finally{Arrays.fill(secret,'\0');}
    }
    static int parsePort(String value){
        if(value==null||!value.matches("[1-9][0-9]{0,4}"))throw new IllegalArgumentException("Ports must be decimal numbers from 1 to 65535");
        int port=Integer.parseInt(value);validatePort(port);return port;
    }
    static void validatePort(int port){if(port<1||port>65535)throw new IllegalArgumentException("Ports must be from 1 to 65535");}
    static void validateHost(String ip){
        if(ip==null||ip.length()>15)throw new IllegalArgumentException("Enter the Windows PC’s numeric IPv4 address");
        String[] parts=ip.split("\\.",-1);
        if(parts.length!=4)throw new IllegalArgumentException("Enter the Windows PC’s numeric IPv4 address");
        int[] octets=new int[4];
        for(int i=0;i<4;i++){String p=parts[i];if(!p.matches("0|[1-9][0-9]{0,2}"))throw new IllegalArgumentException("Windows IP must contain four decimal numbers, without spaces or leading zeros");octets[i]=Integer.parseInt(p);if(octets[i]>255)throw new IllegalArgumentException("Invalid Windows IPv4 address");}
        if(octets[0]==0||octets[0]==127||octets[0]>=224)throw new IllegalArgumentException("Use the Windows PC’s LAN IP, not a loopback, broadcast or multicast address");
    }
    PairingConfig copy(){return new PairingConfig(host,hostPort,localPort,key,channel);}
    void erase(){Arrays.fill(key,'\0');}
    String endpoint(){return host+":"+hostPort+" · phone UDP "+localPort;}
}
