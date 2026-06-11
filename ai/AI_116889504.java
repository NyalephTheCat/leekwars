import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_116889504 extends AI {
public AI_116889504() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_a() throws LeekRunException {
ops(1);return f_b();
}
private Object f_b() throws LeekRunException {
ops(1);return f_c();
}
private Object f_c() throws LeekRunException {
ops(1);return f_d();
}
private Object f_d() throws LeekRunException {
ops(1);return 100l;
}
public Object runIA(Session session) throws LeekRunException {
return f_a();
}
protected String getAIString() { return "Main_1a99d04c56d2f_17";}
protected String[] getErrorFiles() { return new String[] {"A", "B", "C", "D", "Main_1a99d04c56d2f_17", };}

protected int[] getErrorFilesID() { return new int[] {1026, 1027, 1028, 1029, 116889504, };}

}
