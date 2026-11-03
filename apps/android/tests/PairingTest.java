package dev.roomsync;
import java.util.Arrays;
public final class PairingTest {
    static void check(boolean b){if(!b)throw new AssertionError();}
    static void bad(String s){try{PairingConfig.parse(s,0);throw new AssertionError("Accepted invalid setup code");}catch(IllegalArgumentException expected){}}
    public static void main(String[] args){
        char[] raw=new char[64];Arrays.fill(raw,'a');String key=new String(raw),code="RS1|192.168.1.8|5000|5001|"+key;
        PairingConfig p=PairingConfig.parse(" \n"+code+"\r\n",1);check(p.host.equals("192.168.1.8")&&p.hostPort==5000&&p.localPort==5001&&p.channel==1);
        PairingMemory memory=new PairingMemory();memory.store(p);p.erase();PairingConfig copy=memory.copy();check(copy.key[0]=='a');copy.erase();check(memory.copy().key[0]=='a');memory.clear();check(!memory.available()&&memory.copy()==null);
        for(String host:new String[]{"0.0.0.0","127.0.0.1","224.0.0.1","255.255.255.255","192.168.001.8","256.1.1.1","example.com","192.168.1.8@evil","192.168.1.8:80"})bad("RS1|"+host+"|5000|5001|"+key);
        for(String port:new String[]{"0","65536","-1","+5000","05000","5000.0"," 5000"})bad("RS1|192.168.1.8|"+port+"|5001|"+key);
        bad(code+"|extra");bad(code.replace("RS1","RS2"));bad(code.substring(0,code.length()-1));bad(code.replace("aaaa","a aa"));bad("roomsync://pair?key="+key);bad(code+"\ninside");bad(new String(new char[257]));
        PairingConfig upper=PairingConfig.parse(code.toUpperCase(),0);upper.erase();
        System.out.println("PairingTest: strict RS1 grammar, bounds, endpoint validation, whitespace, secret-copy isolation and forgetting passed");
    }
}
