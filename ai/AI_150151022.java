import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_150151022 extends AI {
public AI_150151022() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_f() throws LeekRunException {
ops(1);return f_g();
}
private Object f_g() throws LeekRunException {
ops(1);return 9l;
}
public Object runIA(Session session) throws LeekRunException {
return f_f();
}
protected String getAIString() { return "Main_1a99cf0ed4420_8";}
protected String[] getErrorFiles() { return new String[] {"A", "B", "Main_1a99cf0ed4420_8", };}

protected int[] getErrorFilesID() { return new int[] {1026, 1027, 150151022, };}

}
