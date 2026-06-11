import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4819009 extends AI {
public AI_4819009() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_a() throws LeekRunException {
ops(1);return 1l;
}
private Object f_b() throws LeekRunException {
ops(1);return 99l;
}
public Object runIA(Session session) throws LeekRunException {
return f_a();
}
protected String getAIString() { return "Main_1a99cf96e0929_11";}
protected String[] getErrorFiles() { return new String[] {"A", "B", "Main_1a99cf96e0929_11", };}

protected int[] getErrorFilesID() { return new int[] {1026, 1027, 4819009, };}

}
