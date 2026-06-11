import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_148714879 extends AI {
public AI_148714879() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_f() throws LeekRunException {
ops(1);ops(1); return (Object) add(f_g(), 2l);
}
private Object f_g() throws LeekRunException {
ops(1);return 7l;
}
public Object runIA(Session session) throws LeekRunException {
return f_f();
}
protected String getAIString() { return "Main_1a99cf605b25c_10";}
protected String[] getErrorFiles() { return new String[] {"A", "B", "Main_1a99cf605b25c_10", };}

protected int[] getErrorFilesID() { return new int[] {1026, 1027, 148714879, };}

}
