package dev.roomsync;

import android.Manifest;
import android.app.*;
import android.content.*;
import android.content.pm.PackageManager;
import android.content.res.ColorStateList;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.*;
import android.text.InputFilter;
import android.text.InputType;
import android.view.*;
import android.widget.*;
import java.util.Arrays;

/** One connection action on the home screen; technical controls stay out of the way. */
public final class MainActivity extends Activity {
    private static final int INK=0xff171717,MUTED=0xff666666,LINE=0xffe5e5e5,PAPER=0xfffafafa;
    private final Handler handler=new Handler(Looper.getMainLooper());
    private TextView badge,title,detail,computer,footer;
    private Button primary,secondary;
    private ProgressBar progress;
    private AlertDialog activeDialog;
    private TextView diagnosticText;
    private String selectedHost;
    private int selectedChannel;
    private boolean scanInFlight;
    private final Runnable refresh=new Runnable(){@Override public void run(){render();handler.postDelayed(this,500);}};

    @Override public void onCreate(Bundle state){
        super.onCreate(state);if(state!=null){int choice=state.getInt("channel",0);if(choice>=0&&choice<=3)selectedChannel=choice;}getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
        PairingConfig saved=ReceiverService.pairing.copy();if(saved!=null){selectedHost=saved.host;selectedChannel=saved.channel;saved.erase();}
        ScrollView scroll=new ScrollView(this);scroll.setFillViewport(true);scroll.setBackgroundColor(PAPER);
        LinearLayout body=column();body.setPadding(dp(24),dp(24),dp(24),dp(24));scroll.addView(body,new ScrollView.LayoutParams(-1,-1));
        LinearLayout top=new LinearLayout(this);top.setGravity(Gravity.CENTER_VERTICAL);body.addView(top);
        TextView name=label("RoomSync",22,INK);name.setTypeface(Typeface.create("sans-serif-medium",Typeface.NORMAL));top.addView(name,new LinearLayout.LayoutParams(0,-2,1));
        Button settings=button("Settings",false);settings.setMinWidth(dp(88));settings.setPadding(dp(12),0,dp(12),0);top.addView(settings,new LinearLayout.LayoutParams(-2,-2));settings.setOnClickListener(v->settings());
        space(body,48);
        LinearLayout statusLine=new LinearLayout(this);statusLine.setGravity(Gravity.CENTER_VERTICAL);body.addView(statusLine);
        badge=label("Not connected",13,MUTED);badge.setPadding(dp(10),dp(6),dp(10),dp(6));badge.setBackground(surface(Color.WHITE,LINE,20));statusLine.addView(badge);
        progress=new ProgressBar(this,null,android.R.attr.progressBarStyleSmall);progress.setIndeterminateTintList(ColorStateList.valueOf(INK));LinearLayout.LayoutParams p=new LinearLayout.LayoutParams(dp(18),dp(18));p.leftMargin=dp(12);statusLine.addView(progress,p);
        space(body,20);title=label("Connect your computer",28,INK);title.setTypeface(Typeface.create("sans-serif-medium",Typeface.NORMAL));title.setLineSpacing(dp(2),1f);body.addView(title);
        space(body,12);detail=label("Open RoomSync on Windows, then scan its QR code.",16,MUTED);detail.setLineSpacing(dp(4),1f);detail.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_POLITE);body.addView(detail);
        space(body,20);computer=label("",14,MUTED);body.addView(computer);
        space(body,28);primary=button("Scan QR code",true);primary.setMinHeight(dp(54));body.addView(primary,new LinearLayout.LayoutParams(-1,-2));primary.setOnClickListener(v->primaryAction());
        space(body,12);secondary=button("Paste setup code",false);body.addView(secondary,new LinearLayout.LayoutParams(-1,-2));secondary.setOnClickListener(v->{if(!ReceiverService.running){if(ReceiverService.pairing.available())changeComputer();else pasteCode();}});
        View filler=new View(this);body.addView(filler,new LinearLayout.LayoutParams(1,0,1));space(body,48);
        View rule=new View(this);rule.setBackgroundColor(LINE);body.addView(rule,new LinearLayout.LayoutParams(-1,dp(1)));space(body,16);
        footer=label("Same Wi-Fi. No account needed.",13,MUTED);body.addView(footer);
        Button details=button("Connection details",false);details.setBackgroundColor(Color.TRANSPARENT);details.setPadding(0,0,0,0);details.setGravity(Gravity.START|Gravity.CENTER_VERTICAL);body.addView(details,new LinearLayout.LayoutParams(-1,-2));details.setOnClickListener(v->diagnostics());
        setContentView(scroll);
        if(Build.VERSION.SDK_INT>=35)body.setOnApplyWindowInsetsListener((v,insets)->{android.graphics.Insets i=insets.getInsets(WindowInsets.Type.systemBars());v.setPadding(dp(24)+i.left,dp(24)+i.top,dp(24)+i.right,dp(24)+i.bottom);return insets;});
        render();
    }
    private void render(){
        boolean running=ReceiverService.running,paired=ReceiverService.pairing.available();String raw=ReceiverService.status;
        UiState ui=UiState.from(raw,running,paired);setText(badge,ui.badge);setText(title,ui.title);setText(detail,ui.detail);
        badge.setTextColor(ui.warning?0xff9a3412:MUTED);badge.setBackground(surface(ui.warning?0xfffff7ed:Color.WHITE,ui.warning?0xfffed7aa:LINE,20));
        progress.setVisibility(ui.busy?View.VISIBLE:View.GONE);
        computer.setVisibility(paired&&selectedHost!=null?View.VISIBLE:View.GONE);setText(computer,"Windows · "+(selectedHost==null?"":selectedHost));
        boolean stopping=running&&raw.startsWith("Stopping");setText(primary,stopping?"Disconnecting…":running?"Disconnect":paired?"Connect":"Scan QR code");primary.setEnabled(!stopping&&!scanInFlight);secondary.setEnabled(!scanInFlight);
        secondary.setVisibility(running?View.GONE:View.VISIBLE);setText(secondary,paired?"Change computer":"Paste setup code");
        setText(footer,paired?new String[]{"Stereo audio","Left channel","Right channel","Mono audio"}[selectedChannel]:"Same Wi-Fi. No account needed.");
        if(diagnosticText!=null)setText(diagnosticText,ReceiverService.diagnostics());
    }
    private void primaryAction(){
        if(ReceiverService.running){ReceiverService.requestStop(this,"Stopped by you. Tap Connect to reconnect.");render();return;}
        if(!ReceiverService.pairing.available()){scan();return;}
        PairingConfig saved=ReceiverService.pairing.copy();if(saved==null){render();return;}
        PairingConfig config=null;
        try{config=new PairingConfig(saved.host,saved.hostPort,saved.localPort,saved.key,selectedChannel);ReceiverService.begin(this,config);config=null;
            if(Build.VERSION.SDK_INT>=33&&checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)!=PackageManager.PERMISSION_GRANTED)requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS},1);
        }catch(Exception e){showError(e.getMessage());}finally{saved.erase();if(config!=null)config.erase();render();}
    }
    private void scan(){if(!ReceiverService.running&&!scanInFlight){scanInFlight=true;try{startActivityForResult(new Intent(this,ScanActivity.class),21);}catch(RuntimeException e){scanInFlight=false;showError("The QR scanner could not open. Try Paste setup code instead.");}render();}}
    private void changeComputer(){
        if(ReceiverService.running)return;
        show(new AlertDialog.Builder(this).setTitle("Change computer").setItems(new String[]{"Scan QR code","Paste setup code","Enter manually"},(d,which)->{d.dismiss();if(which==0)scan();else if(which==1)pasteCode();else manualSetup();}).setNegativeButton("Cancel",null).create(),null);
    }
    private void pasteCode(){
        if(ReceiverService.running)return;
        LinearLayout body=dialogBody();body.addView(label("Copy the setup code from RoomSync on Windows.",15,MUTED));space(body,16);
        EditText code=field(body,"Setup code","Paste your private setup code",InputType.TYPE_CLASS_TEXT|InputType.TYPE_TEXT_VARIATION_PASSWORD,PairingConfig.MAX_CODE_LENGTH);
        ScrollView pasteScroll=new ScrollView(this);pasteScroll.addView(body);
        AlertDialog dialog=new AlertDialog.Builder(this).setTitle("Paste setup code").setView(pasteScroll).setNegativeButton("Cancel",null).setPositiveButton("Use code",null).create();
        show(dialog,()->code.getText().clear());dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(v->{
            try{if(ReceiverService.running)throw new IllegalArgumentException("Disconnect before changing your computer.");PairingConfig config=PairingConfig.parse(code.getText().toString(),selectedChannel);try{load(config);}finally{config.erase();}dialog.dismiss();}
            catch(IllegalArgumentException e){code.setError(e.getMessage());}
        });
    }
    private void load(PairingConfig config){ReceiverService.pairing.store(config);selectedHost=config.host;selectedChannel=config.channel;ReceiverService.status="Setup loaded. Tap Connect when you’re ready.";render();}
    private void manualSetup(){
        if(ReceiverService.running)return;
        PairingConfig saved=ReceiverService.pairing.copy();LinearLayout body=dialogBody();body.addView(label("Use the connection details shown by Windows.",15,MUTED));space(body,12);
        EditText host=field(body,"Windows IP address","192.168.1.8",InputType.TYPE_CLASS_TEXT,15);
        EditText hostPort=field(body,"Windows port","5000",InputType.TYPE_CLASS_NUMBER,5);hostPort.setText("5000");
        EditText phonePort=field(body,"Phone port","5001",InputType.TYPE_CLASS_NUMBER,5);phonePort.setText("5001");
        EditText key=field(body,"Pairing key",saved==null?"64 hexadecimal characters":"Already saved for this computer",InputType.TYPE_CLASS_TEXT|InputType.TYPE_TEXT_VARIATION_PASSWORD,64);
        if(saved!=null){host.setText(saved.host);hostPort.setText(Integer.toString(saved.hostPort));phonePort.setText(Integer.toString(saved.localPort));}
        TextView error=label("",14,0xffb42318);error.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_ASSERTIVE);error.setFocusableInTouchMode(true);body.addView(error);
        ScrollView scroll=new ScrollView(this);scroll.addView(body);
        AlertDialog dialog=new AlertDialog.Builder(this).setTitle("Manual setup").setView(scroll).setNegativeButton("Cancel",null).setPositiveButton("Save",null).create();
        show(dialog,()->{key.getText().clear();if(saved!=null)saved.erase();});
        dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(v->{char[] secret=null;PairingConfig config=null;
            try{if(ReceiverService.running)throw new IllegalArgumentException("Disconnect before changing your computer.");String ip=host.getText().toString().trim();int hp=PairingConfig.parsePort(hostPort.getText().toString()),lp=PairingConfig.parsePort(phonePort.getText().toString());String typed=key.getText().toString().trim();
                if(typed.isEmpty()){if(saved==null||!saved.host.equals(ip)||saved.hostPort!=hp||saved.localPort!=lp)throw new IllegalArgumentException("Enter the key for these connection details.");secret=saved.key.clone();}else secret=typed.toCharArray();
                config=new PairingConfig(ip,hp,lp,secret,selectedChannel);load(config);dialog.dismiss();
            }catch(IllegalArgumentException e){error.setText(e.getMessage());error.requestFocus();}finally{if(secret!=null)Arrays.fill(secret,'\0');if(config!=null)config.erase();}
        });
    }
    private void settings(){
        LinearLayout body=dialogBody();TextView audioLabel=label("Audio channel",14,INK);body.addView(audioLabel);
        Spinner channels=new Spinner(this);channels.setMinimumHeight(dp(52));channels.setId(View.generateViewId());audioLabel.setLabelFor(channels.getId());channels.setAdapter(new ArrayAdapter<String>(this,android.R.layout.simple_spinner_dropdown_item,new String[]{"Stereo","Left channel","Right channel","Mono"}));channels.setSelection(selectedChannel);channels.setEnabled(!ReceiverService.running);body.addView(channels,new LinearLayout.LayoutParams(-1,-2));
        body.addView(label(ReceiverService.running?"Disconnect to change the audio channel.":"Applied the next time you connect.",13,MUTED));space(body,20);
        Button manual=button("Manual connection setup",false);manual.setEnabled(!ReceiverService.running);body.addView(manual,new LinearLayout.LayoutParams(-1,-2));
        space(body,8);Button forget=button("Forget computer",false);forget.setEnabled(!ReceiverService.running&&ReceiverService.pairing.available());body.addView(forget,new LinearLayout.LayoutParams(-1,-2));
        space(body,20);body.addView(label("Setup stays in this app’s memory. No account, microphone or telemetry.",13,MUTED));space(body,8);String version="";try{version=getPackageManager().getPackageInfo(getPackageName(),0).versionName;}catch(PackageManager.NameNotFoundException ignored){}body.addView(label("RoomSync "+version,12,MUTED));
        ScrollView settingsScroll=new ScrollView(this);settingsScroll.addView(body);
        AlertDialog dialog=new AlertDialog.Builder(this).setTitle("Settings").setView(settingsScroll).setNegativeButton("Cancel",null).setPositiveButton("Done",(d,w)->{if(!ReceiverService.running){selectedChannel=channels.getSelectedItemPosition();rememberChannel();render();}}).create();
        show(dialog,null);manual.setOnClickListener(v->{dialog.dismiss();manualSetup();});forget.setOnClickListener(v->{dialog.dismiss();confirmForget();});
    }
    private void rememberChannel(){PairingConfig saved=ReceiverService.pairing.copy();if(saved==null)return;PairingConfig updated=null;try{updated=new PairingConfig(saved.host,saved.hostPort,saved.localPort,saved.key,selectedChannel);ReceiverService.pairing.store(updated);}finally{saved.erase();if(updated!=null)updated.erase();}}
    private void confirmForget(){show(new AlertDialog.Builder(this).setTitle("Forget this computer?").setMessage("You can connect again by scanning its QR code.").setNegativeButton("Cancel",null).setPositiveButton("Forget",(d,w)->{if(ReceiverService.running)return;ReceiverService.pairing.clear();selectedHost=null;selectedChannel=0;ReceiverService.status="Pairing forgotten.";render();}).create(),null);}
    private void diagnostics(){
        LinearLayout body=dialogBody();body.addView(label("Use these details when troubleshooting. Your pairing key is never included.",14,MUTED));space(body,16);
        diagnosticText=label(ReceiverService.diagnostics(),12,MUTED);diagnosticText.setTypeface(Typeface.MONOSPACE);diagnosticText.setTextIsSelectable(true);body.addView(diagnosticText);space(body,16);body.addView(label("Both devices need the same local network. A VPN, guest Wi-Fi or firewall can prevent a connection. Bluetooth can add delay.",13,MUTED));
        ScrollView scroll=new ScrollView(this);scroll.addView(body);
        AlertDialog dialog=new AlertDialog.Builder(this).setTitle("Connection details").setView(scroll).setNegativeButton("Close",null).setPositiveButton("Copy details",null).create();show(dialog,()->diagnosticText=null);
        dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(v->{((ClipboardManager)getSystemService(CLIPBOARD_SERVICE)).setPrimaryClip(ClipData.newPlainText("RoomSync diagnostics",ReceiverService.diagnostics()));Toast.makeText(this,"Details copied. No pairing key included.",Toast.LENGTH_SHORT).show();});
    }
    private void showError(String message){show(new AlertDialog.Builder(this).setTitle("Couldn’t connect").setMessage(message==null?"Please try again.":message).setPositiveButton("OK",null).create(),null);}
    private void show(AlertDialog dialog,Runnable cleanup){if(activeDialog!=null)activeDialog.dismiss();activeDialog=dialog;dialog.setOnDismissListener(d->{if(cleanup!=null)cleanup.run();if(activeDialog==dialog)activeDialog=null;});dialog.show();if(dialog.getWindow()!=null)dialog.getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);}
    @Override protected void onActivityResult(int request,int result,Intent data){super.onActivityResult(request,result,data);if(request==21)scanInFlight=false;if(request==21&&result==RESULT_OK&&data!=null){String code=data.getStringExtra("setup");data.removeExtra("setup");if(code!=null&&!ReceiverService.running){try{PairingConfig config=PairingConfig.parse(code,selectedChannel);try{load(config);}finally{config.erase();}}catch(IllegalArgumentException e){showError(e.getMessage());}}}}
    private LinearLayout column(){LinearLayout b=new LinearLayout(this);b.setOrientation(LinearLayout.VERTICAL);return b;}
    private LinearLayout dialogBody(){LinearLayout b=column();b.setPadding(dp(24),dp(12),dp(24),dp(12));return b;}
    private TextView label(String value,int size,int color){TextView t=new TextView(this);t.setText(value);t.setTextSize(size);t.setTextColor(color);t.setFontFeatureSettings("kern");return t;}
    private Button button(String label,boolean filled){Button b=new Button(this);b.setText(label);b.setTextSize(15);b.setAllCaps(false);b.setSingleLine(false);b.setMaxLines(3);b.setTypeface(Typeface.create("sans-serif-medium",Typeface.NORMAL));b.setTextColor(new ColorStateList(new int[][]{new int[]{-android.R.attr.state_enabled},new int[]{}},new int[]{0xff999999,filled?Color.WHITE:INK}));b.setBackground(new android.graphics.drawable.RippleDrawable(ColorStateList.valueOf(filled?0x33ffffff:0x11000000),surface(filled?INK:Color.WHITE,filled?INK:LINE,10),null));b.setPadding(dp(16),dp(12),dp(16),dp(12));b.setMinHeight(dp(48));b.setStateListAnimator(null);return b;}
    private GradientDrawable surface(int fill,int border,int radius){GradientDrawable d=new GradientDrawable();d.setColor(fill);d.setCornerRadius(dp(radius));d.setStroke(dp(1),border);return d;}
    private EditText field(LinearLayout parent,String name,String hint,int input,int limit){TextView fieldLabel=label(name,13,MUTED);parent.addView(fieldLabel);space(parent,6);EditText e=new EditText(this);e.setId(View.generateViewId());fieldLabel.setLabelFor(e.getId());e.setTextSize(16);e.setSingleLine(true);e.setInputType(input);e.setHint(hint);e.setTextColor(INK);e.setHintTextColor(MUTED);e.setBackground(surface(Color.WHITE,LINE,8));e.setPadding(dp(12),dp(12),dp(12),dp(12));e.setSaveEnabled(false);e.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);e.setFilters(new InputFilter[]{new InputFilter.LengthFilter(limit)});parent.addView(e,new LinearLayout.LayoutParams(-1,-2));space(parent,16);return e;}
    private void space(LinearLayout parent,int height){parent.addView(new View(this),new LinearLayout.LayoutParams(1,dp(height)));}
    private static void setText(TextView view,String value){if(!view.getText().toString().equals(value))view.setText(value);}
    private int dp(int n){return Math.round(n*getResources().getDisplayMetrics().density);}
    @Override protected void onResume(){super.onResume();handler.removeCallbacks(refresh);handler.post(refresh);}
    @Override protected void onPause(){handler.removeCallbacks(refresh);super.onPause();}
    @Override protected void onSaveInstanceState(Bundle state){state.putInt("channel",selectedChannel);super.onSaveInstanceState(state);}
    @Override protected void onDestroy(){handler.removeCallbacks(refresh);if(activeDialog!=null)activeDialog.dismiss();super.onDestroy();}
}
