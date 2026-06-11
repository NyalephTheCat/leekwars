import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_200102189 extends AI {
public AI_200102189() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_f() throws LeekRunException {
ops(1);return 2l;
}
public Object runIA(Session session) throws LeekRunException {
return f_f();
}
protected String getAIString() { return "Main_1a99d13dec9f3_23";}
protected String[] getErrorFiles() { return new String[] {"sub", "Main_1a99d13dec9f3_23", };}

protected int[] getErrorFilesID() { return new int[] {115201, 200102189, };}

}
