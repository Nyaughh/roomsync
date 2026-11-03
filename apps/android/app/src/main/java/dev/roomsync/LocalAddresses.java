package dev.roomsync;

import java.net.*;
import java.util.*;

final class LocalAddresses {
    static String describe(){
        List<String> entries=new ArrayList<>();
        try{
            Enumeration<NetworkInterface> interfaces=NetworkInterface.getNetworkInterfaces();
            if(interfaces!=null)while(interfaces.hasMoreElements()&&entries.size()<8){NetworkInterface n=interfaces.nextElement();if(!n.isUp()||n.isLoopback())continue;Enumeration<InetAddress> addresses=n.getInetAddresses();while(addresses.hasMoreElements()&&entries.size()<8){InetAddress a=addresses.nextElement();if(a instanceof Inet4Address&&!a.isLoopbackAddress()&&!a.isAnyLocalAddress())entries.add(a.getHostAddress()+" ("+n.getName()+")");}}
        }catch(Exception ignored){return "Local IP unavailable. Check Wi-Fi settings.";}
        Collections.sort(entries);
        return entries.isEmpty()?"No active IPv4 address. Connect to the same Wi-Fi as Windows.":String.join("\n",entries);
    }
}
